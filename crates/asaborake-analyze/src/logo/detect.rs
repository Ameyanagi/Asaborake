//! Scoring frames against a learned logo.
//!
//! # Why not just subtract
//!
//! The obvious test — remove the logo and see whether the picture got simpler
//! — fails, because how much a pixel changes depends on what was behind it. A
//! logo over black barely moves the numbers; the same logo over white moves
//! them a lot. Thresholding raw differences therefore tracks the brightness of
//! the programme rather than the presence of the logo.
//!
//! # What is done instead
//!
//! Correlation against the logo's *shape*, measured locally and normalised:
//!
//! 1. Composite the logo onto a ladder of uniform grey backgrounds, giving a
//!    reference image of the logo at every brightness it might appear over.
//! 2. Pick the pixels whose 5x5 neighbourhood varies most — the logo's edges.
//!    Flat interior pixels carry no shape information.
//! 3. For each such pixel, keep the zero-mean 5x5 patch as a kernel. Zero-mean
//!    is what makes the match indifferent to the background's brightness.
//! 4. Precompute, per pixel and per background level, the correlation the
//!    reference image itself produces, and normalise by it — so a perfect
//!    match scores 1 regardless of where on the ladder it lands.
//!
//! At detection time each frame is scored twice: once as it is, and once with
//! the logo removed. A frame carrying the logo correlates strongly as-is and
//! near zero once removed. A frame without it correlates near zero as-is and
//! *negatively* once removed, because removing an absent logo stamps its
//! photographic negative into the picture. Combining the two is far sharper
//! than either alone.
//!
//! This is the scheme in Amatsukaze's `LogoScan.hpp`; see `ATTRIBUTION.md`.

use super::model::LogoData;

/// Side of the correlation kernel.
const KERNEL: usize = 5;
/// Pixels in one kernel.
const KERNEL_LEN: usize = KERNEL * KERNEL;
/// Half-width of the kernel, i.e. the border it cannot reach into.
const KERNEL_RADIUS: u32 = 2;

/// Number of background brightness levels the logo is referenced against.
const LEVELS: usize = 32;

/// Fraction of the logo rectangle used as correlation points.
const MASK_RATIO: f32 = 0.25;

/// Ceiling on correlation points, to bound the per-frame cost.
///
/// A large rectangle would otherwise make scoring quadratic in logo size for
/// no gain: the edges carry the signal, and there are only so many of them.
const MAX_MASK_POINTS: usize = 1500;

/// Correlations below this fraction of the average are treated as carrying no
/// logo information, and are faded out rather than amplified by normalisation.
const CORRELATION_FLOOR: f32 = 0.2;

/// Per-point, per-background normalisation.
#[derive(Debug, Clone, Copy)]
struct Scale {
    /// Reciprocal of the reference correlation, so a match normalises to 1.
    factor: f32,
    /// Confidence weight for points whose reference correlation was weak.
    weight: f32,
}

/// A logo compiled into the form the per-frame scorer needs.
#[derive(Debug)]
pub struct LogoDetector {
    logo: LogoData,
    /// Correlation points, as offsets within the logo rectangle.
    points: Vec<(u32, u32)>,
    /// Zero-mean kernel per point, `KERNEL_LEN` values each.
    kernels: Vec<f32>,
    /// `points.len() * LEVELS` normalisation entries.
    scales: Vec<Scale>,
    /// Score the reference logo achieves, used to normalise to roughly 1.
    reference_score: f32,
    /// Scratch buffers, reused across frames.
    work: Vec<f32>,
    removed: Vec<f32>,
}

impl LogoDetector {
    /// Compile a logo for detection.
    ///
    /// Returns `None` when the rectangle is too small to hold a kernel, or
    /// when the logo carries no correlatable shape.
    #[must_use]
    pub fn new(logo: LogoData) -> Option<Self> {
        let (width, height) = (logo.rect.width, logo.rect.height);
        if width <= KERNEL as u32 || height <= KERNEL as u32 {
            return None;
        }

        let references = build_reference_ladder(&logo);
        let points = select_points(&logo, &references)?;

        let mut kernels = Vec::with_capacity(points.len() * KERNEL_LEN);
        // The darkest background gives the cleanest view of the logo's own
        // shape, with the least of the background bleeding into it.
        let darkest = references.first()?;
        for &(x, y) in &points {
            kernels.extend_from_slice(&zero_mean_patch(darkest, width, x, y));
        }

        let mut scales = vec![
            Scale {
                factor: 0.0,
                weight: 0.0
            };
            points.len() * LEVELS
        ];
        let mut total_correlation = 0.0f32;
        for (index, &(x, y)) in points.iter().enumerate() {
            let kernel = kernels.get(index * KERNEL_LEN..(index + 1) * KERNEL_LEN)?;
            for (level, reference) in references.iter().enumerate() {
                let (correlation, _) = correlate(kernel, reference, width, x, y);
                let magnitude = correlation.abs();
                total_correlation += magnitude;
                if let Some(slot) = scales.get_mut(index * LEVELS + level) {
                    slot.factor = magnitude;
                }
            }
        }

        let average = total_correlation / (points.len() * LEVELS) as f32;
        if average <= f32::EPSILON {
            return None;
        }
        let floor = average * CORRELATION_FLOOR;
        for scale in &mut scales {
            let magnitude = scale.factor;
            scale.factor = if magnitude > 0.0 {
                1.0 / magnitude
            } else {
                0.0
            };
            scale.weight = (magnitude / floor).min(1.0);
        }

        let mut detector = Self {
            logo,
            points,
            kernels,
            scales,
            reference_score: 1.0,
            work: vec![0.0; (width * height) as usize],
            removed: vec![0.0; (width * height) as usize],
        };

        // A near-black background is the canonical "logo clearly present"
        // case, and its score sets the scale everything else is read against.
        let reference = references.get(2).or_else(|| references.first())?;
        detector.reference_score = detector.correlation_score(reference);
        if detector.reference_score.abs() < f32::EPSILON {
            return None;
        }

        Some(detector)
    }

    /// The logo this detector was built from.
    #[must_use]
    pub const fn logo(&self) -> &LogoData {
        &self.logo
    }

    /// How many correlation points the logo yielded.
    #[must_use]
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Score one frame: positive when the logo is present, negative when it is
    /// confidently absent, near zero when the frame carries no information.
    pub fn score(&mut self, frame: &asaborake_media::Frame<'_>) -> f32 {
        let rect = self.logo.rect;
        if !rect.fits_within(frame.width, frame.height) {
            return 0.0;
        }

        for row in 0..rect.height {
            let source = ((rect.y + row) * frame.width + rect.x) as usize;
            let target = (row * rect.width) as usize;
            for column in 0..rect.width as usize {
                let observed = frame
                    .luma
                    .get(source + column)
                    .map_or(0.0, |&v| f32::from(v) / 255.0);
                if let Some(slot) = self.work.get_mut(target + column) {
                    *slot = observed;
                }
                if let Some(slot) = self.removed.get_mut(target + column) {
                    *slot = self.logo.remove(target + column, observed);
                }
            }
        }

        let present = self.correlation_score(&self.work) / self.reference_score;
        let absent = self.correlation_score(&self.removed) / self.reference_score;

        // Present: `present` is strongly positive and `absent` collapses to
        // zero. Absent: `present` is noise around zero and `absent` goes
        // negative. Taking the positive part of one and the negative part of
        // the other suppresses the noise in each.
        present.max(0.0) + absent.min(0.0)
    }

    /// Total normalised correlation of an image against the logo's kernels.
    fn correlation_score(&self, image: &[f32]) -> f32 {
        let width = self.logo.rect.width;
        let mut total = 0.0f32;

        for (index, &(x, y)) in self.points.iter().enumerate() {
            let Some(kernel) = self
                .kernels
                .get(index * KERNEL_LEN..(index + 1) * KERNEL_LEN)
            else {
                continue;
            };
            let (correlation, average) = correlate(kernel, image, width, x, y);

            // Which rung of the reference ladder this neighbourhood sits on.
            let level = ((average * 255.0).clamp(0.0, 255.0) as usize >> 3).min(LEVELS - 1);
            let Some(scale) = self.scales.get(index * LEVELS + level) else {
                continue;
            };

            // Clamping discards correlation beyond what the logo alone could
            // produce; anything above 1 came from the picture, not the logo.
            let normalised = (correlation * scale.factor).clamp(-1.0, 1.0);
            total += normalised * scale.weight;
        }

        total
    }
}

/// Composite the logo onto a ladder of uniform grey backgrounds.
fn build_reference_ladder(logo: &LogoData) -> Vec<Vec<f32>> {
    (0..LEVELS)
        .map(|level| {
            let background = (level * 8) as f32 / 255.0;
            (0..logo.rect.area())
                .map(|index| logo.apply(index, background))
                .collect()
        })
        .collect()
}

/// Choose the pixels whose neighbourhoods carry the most shape.
fn select_points(logo: &LogoData, references: &[Vec<f32>]) -> Option<Vec<(u32, u32)>> {
    let (width, height) = (logo.rect.width, logo.rect.height);
    // The middle of the ladder is the least biased view: the logo is neither
    // washed out against white nor lost against black.
    let middle = references.get(LEVELS / 2)?;

    let mut ranked: Vec<(f32, u32, u32)> = Vec::new();
    for y in KERNEL_RADIUS..height - KERNEL_RADIUS {
        for x in KERNEL_RADIUS..width - KERNEL_RADIUS {
            let patch = zero_mean_patch(middle, width, x, y);
            let energy: f32 = patch.iter().map(|v| v * v).sum();
            if energy > 0.0 {
                ranked.push((energy, x, y));
            }
        }
    }
    if ranked.is_empty() {
        return None;
    }

    ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let wanted = (((width * height) as f32 * MASK_RATIO) as usize)
        .clamp(1, MAX_MASK_POINTS)
        .min(ranked.len());

    Some(
        ranked
            .into_iter()
            .take(wanted)
            .map(|(_, x, y)| (x, y))
            .collect(),
    )
}

/// The 5x5 neighbourhood at `(x, y)`, with its mean removed.
fn zero_mean_patch(image: &[f32], width: u32, x: u32, y: u32) -> [f32; KERNEL_LEN] {
    let mut patch = [0.0f32; KERNEL_LEN];
    let total = gather_patch(image, width, x, y, &mut patch);
    let mean = total / KERNEL_LEN as f32;
    for value in &mut patch {
        *value -= mean;
    }
    patch
}

/// Copy the neighbourhood centred on `(x, y)` into `patch`, returning its sum.
///
/// Coordinates stay unsigned throughout: the offset is applied by adding the
/// loop index and subtracting the radius, which saturates at the frame edge
/// exactly as clamping a signed offset would.
fn gather_patch(image: &[f32], width: u32, x: u32, y: u32, patch: &mut [f32; KERNEL_LEN]) -> f32 {
    let mut total = 0.0f32;
    for row in 0..KERNEL {
        let sy = (y + row as u32).saturating_sub(KERNEL_RADIUS);
        for column in 0..KERNEL {
            let sx = (x + column as u32).saturating_sub(KERNEL_RADIUS);
            let value = image
                .get((sy * width + sx) as usize)
                .copied()
                .unwrap_or(0.0);
            patch[row * KERNEL + column] = value;
            total += value;
        }
    }
    total
}

/// Correlate a zero-mean kernel with an image neighbourhood.
///
/// Returns the correlation and the neighbourhood's mean, the latter being what
/// selects the reference rung to normalise against.
fn correlate(kernel: &[f32], image: &[f32], width: u32, x: u32, y: u32) -> (f32, f32) {
    let mut values = [0.0f32; KERNEL_LEN];
    let total = gather_patch(image, width, x, y, &mut values);
    let mean = total / KERNEL_LEN as f32;

    let mut correlation = 0.0f32;
    for (k, v) in kernel.iter().zip(&values) {
        correlation = k.mul_add(v - mean, correlation);
    }
    (correlation, mean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logo::model::Rect;
    use asaborake_media::Frame;

    const FRAME_W: u32 = 96;
    const FRAME_H: u32 = 64;

    fn rect() -> Rect {
        Rect {
            x: 8,
            y: 8,
            width: 32,
            height: 24,
        }
    }

    /// A logo shaped like a hollow ring, so it has real edges to correlate.
    fn ring_logo() -> LogoData {
        let r = rect();
        let mut a = vec![1.0f32; r.area()];
        let mut b = vec![0.0f32; r.area()];
        let (alpha, colour) = (0.6f32, 0.95f32);

        for y in 0..r.height {
            for x in 0..r.width {
                let dx = f32::from(x as u16) - f32::from(r.width as u16) / 2.0;
                let dy = f32::from(y as u16) - f32::from(r.height as u16) / 2.0;
                let radius = dx.hypot(dy);
                if (6.0..9.0).contains(&radius) {
                    let index = (y * r.width + x) as usize;
                    a[index] = 1.0 / (1.0 - alpha);
                    b[index] = -alpha * colour / (1.0 - alpha);
                }
            }
        }

        LogoData {
            name: "ring".into(),
            channel_id: None,
            source_width: FRAME_W,
            source_height: FRAME_H,
            rect: r,
            a,
            b,
            frames_used: 500,
        }
    }

    /// Build a frame of `background`, optionally with the logo composited in,
    /// plus some texture so the frame is not perfectly flat.
    fn frame_bytes(logo: &LogoData, background: u8, with_logo: bool, texture: bool) -> Vec<u8> {
        let mut luma = vec![background; (FRAME_W * FRAME_H) as usize];
        if texture {
            for (index, value) in luma.iter_mut().enumerate() {
                let wobble = i32::try_from(index % 4096).unwrap_or(0) * 7919 % 21 - 10;
                *value = (i32::from(*value) + wobble).clamp(0, 255) as u8;
            }
        }
        if with_logo {
            let r = logo.rect;
            for y in 0..r.height {
                for x in 0..r.width {
                    let index = (y * r.width + x) as usize;
                    let frame_index = ((r.y + y) * FRAME_W + r.x + x) as usize;
                    let existing = f32::from(luma[frame_index]) / 255.0;
                    let composited = logo.apply(index, existing);
                    luma[frame_index] = (composited * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        luma
    }

    fn frame(luma: &[u8]) -> Frame<'_> {
        Frame {
            index: 0,
            timestamp: 0.0,
            width: FRAME_W,
            height: FRAME_H,
            luma,
        }
    }

    #[test]
    fn compiles_a_logo_into_correlation_points() {
        let detector = LogoDetector::new(ring_logo()).expect("a detectable logo");
        assert!(detector.point_count() > 20, "{}", detector.point_count());
        assert!(detector.point_count() <= MAX_MASK_POINTS);
    }

    #[test]
    fn scores_a_frame_carrying_the_logo_above_one_without() {
        let logo = ring_logo();
        let mut detector = LogoDetector::new(logo.clone()).expect("a detectable logo");

        for background in [20u8, 80, 140, 200] {
            let with = frame_bytes(&logo, background, true, true);
            let without = frame_bytes(&logo, background, false, true);

            let present = detector.score(&frame(&with));
            let absent = detector.score(&frame(&without));

            assert!(
                present > absent,
                "background {background}: present {present} should exceed absent {absent}"
            );
            assert!(
                present > 0.2,
                "background {background}: logo should score clearly positive, got {present}"
            );
            assert!(
                absent < 0.2,
                "background {background}: absence should not score high, got {absent}"
            );
        }
    }

    #[test]
    fn separation_holds_across_the_brightness_range() {
        // The whole point of normalising per background level: a logo over
        // white must be as detectable as one over black.
        let logo = ring_logo();
        let mut detector = LogoDetector::new(logo.clone()).expect("a detectable logo");

        let dark = detector.score(&frame(&frame_bytes(&logo, 16, true, true)));
        let bright = detector.score(&frame(&frame_bytes(&logo, 230, true, true)));
        assert!(dark > 0.2, "dark background scored {dark}");
        assert!(bright > 0.2, "bright background scored {bright}");
    }

    #[test]
    fn a_rectangle_too_small_for_a_kernel_is_rejected() {
        let mut logo = ring_logo();
        logo.rect = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        logo.a.truncate(16);
        logo.b.truncate(16);
        assert!(LogoDetector::new(logo).is_none());
    }

    #[test]
    fn a_logo_with_no_shape_is_rejected() {
        let r = rect();
        let flat = LogoData {
            name: "flat".into(),
            channel_id: None,
            source_width: FRAME_W,
            source_height: FRAME_H,
            rect: r,
            a: vec![1.0; r.area()],
            b: vec![0.0; r.area()],
            frames_used: 500,
        };
        assert!(
            LogoDetector::new(flat).is_none(),
            "a fully transparent logo has nothing to correlate"
        );
    }

    #[test]
    fn zero_mean_patch_sums_to_zero() {
        let image: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let patch = zero_mean_patch(&image, 10, 5, 5);
        let total: f32 = patch.iter().sum();
        assert!(total.abs() < 1e-5, "patch mean was {total}");
    }

    #[test]
    fn correlation_ignores_a_uniform_offset() {
        // Adding a constant to the whole neighbourhood must not change the
        // correlation; that invariance is what makes the score independent of
        // how bright the programme is.
        let image: Vec<f32> = (0..100).map(|i| ((i % 7) as f32) / 7.0).collect();
        let kernel = zero_mean_patch(&image, 10, 5, 5);
        let (base, _) = correlate(&kernel, &image, 10, 5, 5);

        let brighter: Vec<f32> = image.iter().map(|v| v + 0.3).collect();
        let (shifted, mean) = correlate(&kernel, &brighter, 10, 5, 5);

        assert!((base - shifted).abs() < 1e-5, "{base} vs {shifted}");
        assert!(mean > 0.3, "the mean should track the offset, got {mean}");
    }
}
