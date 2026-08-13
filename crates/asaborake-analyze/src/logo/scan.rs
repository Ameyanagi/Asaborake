//! Learning a logo from a recording.
//!
//! # Why flat frames
//!
//! Fitting `background = a * observed + b` per pixel needs, for each pixel,
//! pairs of (what was observed, what the background actually was). The
//! background is exactly what a recording does not tell us — except in frames
//! where the area around the logo happens to be a single flat colour. A fade
//! to black, a title card, a shot of clear sky: in those, the background under
//! the logo is confidently the same colour as the border around it.
//!
//! So the scanner samples the border of the logo rectangle, rejects the frame
//! unless that border is uniform, and otherwise takes the border's colour as
//! the background for every pixel inside. Over a half-hour programme there are
//! usually thousands of such frames, at a spread of brightnesses — and a
//! spread is essential, because two points on a line are what make the slope
//! and the intercept separately identifiable.
//!
//! This is Amatsukaze's method, from `LogoScan.hpp`; see `ATTRIBUTION.md`.

use asaborake_media::Frame;

use super::model::{LogoData, Rect};

/// How much the border of the logo rectangle may vary and still count as flat.
///
/// In 8-bit luma. Broadcast is compressed, so a genuinely flat area still
/// shows a few levels of ringing and mosquito noise; demanding true uniformity
/// would reject every real frame.
pub const DEFAULT_FLATNESS_THRESHOLD: u8 = 12;

/// Fewest accepted frames before a fit is attempted.
pub const MINIMUM_FRAMES: u32 = 50;

/// Fewest distinct background levels before a fit is attempted.
///
/// Every accepted frame having the same background — a programme that only
/// ever fades to black — leaves the slope and intercept indistinguishable, and
/// the regression degenerates. Requiring a spread catches that.
pub const MINIMUM_BACKGROUND_SPREAD: u8 = 24;

/// Running per-pixel regression accumulators.
///
/// `f` is the foreground, i.e. what was observed with the logo on it; `b` is
/// the background the flat border implied. The shared prefix is the point:
/// these are the five running sums a least-squares fit consumes, and naming
/// them anything else would obscure which sum is which.
#[expect(
    clippy::struct_field_names,
    reason = "the fields are the five sums of a least-squares fit"
)]
#[derive(Debug, Clone, Copy, Default)]
struct PixelStats {
    sum_f: f64,
    sum_b: f64,
    sum_f2: f64,
    sum_b2: f64,
    sum_fb: f64,
}

impl PixelStats {
    fn add(&mut self, foreground: f64, background: f64) {
        self.sum_f += foreground;
        self.sum_b += background;
        self.sum_f2 += foreground * foreground;
        self.sum_b2 += background * background;
        self.sum_fb += foreground * background;
    }
}

/// Least-squares line through `n` points, given the usual sums.
///
/// Returns `(slope, intercept)` for `y = slope * x + intercept`, or `None`
/// when the points are degenerate — all identical `x`, in practice.
fn fit_line(
    n: f64,
    sum_x: f64,
    sum_y: f64,
    sum_squares: f64,
    sum_products: f64,
) -> Option<(f64, f64)> {
    let denominator = n * sum_squares - sum_x * sum_x;
    if denominator.abs() < 1e-12 {
        return None;
    }
    let slope = (n * sum_products - sum_x * sum_y) / denominator;
    let intercept = (sum_squares * sum_y - sum_x * sum_products) / denominator;
    if slope.is_finite() && intercept.is_finite() {
        Some((slope, intercept))
    } else {
        None
    }
}

/// Accumulates the statistics a logo fit needs.
#[derive(Debug)]
pub struct LogoScanner {
    rect: Rect,
    flatness_threshold: u8,
    stats: Vec<PixelStats>,
    frames: u32,
    /// Range of background levels seen, to check the fit is identifiable.
    darkest_background: u8,
    brightest_background: u8,
    /// Whether the border must be flat end to end, or merely mostly flat.
    strict: bool,
    /// Reusable buffer for the border samples, to keep the hot loop allocation
    /// free across tens of thousands of frames.
    border: Vec<u8>,
}

impl LogoScanner {
    /// Start scanning for a logo in `rect`.
    #[must_use]
    pub fn new(rect: Rect, flatness_threshold: u8) -> Self {
        Self {
            rect,
            flatness_threshold,
            stats: vec![PixelStats::default(); rect.area()],
            frames: 0,
            darkest_background: u8::MAX,
            brightest_background: u8::MIN,
            strict: false,
            border: Vec::new(),
        }
    }

    /// Start scanning a rectangle somebody drew, judging the border end to end.
    ///
    /// Amatsukaze rejects a frame unless its border runs min-to-max within the
    /// threshold, and for a box drawn round a logo that is right: the border is
    /// a thin ring of genuine background, and a single stray bright pixel means
    /// something has moved into it.
    ///
    /// The tolerant test below exists for the automatic locator, whose
    /// rectangle is the bounding box of everything steady near the logo and can
    /// be hundreds of pixels a side. Applying that tolerance to a hand-drawn box
    /// lets through frames where a fifth of the border is a different colour
    /// entirely, and the background it then infers is a mixture rather than a
    /// colour — which is how a fit over a thousand frames comes back as
    /// structured noise instead of a logo.
    #[must_use]
    pub fn strict(rect: Rect, flatness_threshold: u8) -> Self {
        Self {
            strict: true,
            ..Self::new(rect, flatness_threshold)
        }
    }

    /// The rectangle being scanned.
    #[must_use]
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// How many frames have been accepted.
    #[must_use]
    pub const fn frames_accepted(&self) -> u32 {
        self.frames
    }

    /// The range of background brightnesses the accepted frames covered.
    ///
    /// The single most useful number when a fit fails. Separating a
    /// translucent logo from what is behind it means watching the background
    /// change underneath it; if every usable frame had the same background,
    /// there is one point and no line, and no amount of extra frames helps.
    #[must_use]
    pub fn background_spread(&self) -> u8 {
        self.brightest_background
            .saturating_sub(self.darkest_background)
    }

    /// Offer a frame; returns whether its background was flat enough to use.
    pub fn add_frame(&mut self, frame: &Frame<'_>) -> bool {
        // A degenerate rectangle would underflow when the border is walked,
        // and it can reach here from a hand-written configuration.
        if !self.rect.is_valid() || !self.rect.fits_within(frame.width, frame.height) {
            return false;
        }
        let Some(background) = self.flat_background(frame) else {
            return false;
        };

        self.darkest_background = self.darkest_background.min(background);
        self.brightest_background = self.brightest_background.max(background);

        let background_value = f64::from(background) / 255.0;
        for row in 0..self.rect.height {
            let source = ((self.rect.y + row) * frame.width + self.rect.x) as usize;
            let target = (row * self.rect.width) as usize;
            for column in 0..self.rect.width as usize {
                let (Some(&observed), Some(stats)) = (
                    frame.luma.get(source + column),
                    self.stats.get_mut(target + column),
                ) else {
                    continue;
                };
                stats.add(f64::from(observed) / 255.0, background_value);
            }
        }

        self.frames += 1;
        true
    }

    /// The background colour implied by the rectangle's border, or `None` when
    /// the border is not flat enough to imply one.
    fn flat_background(&mut self, frame: &Frame<'_>) -> Option<u8> {
        self.border.clear();

        let (left, top) = (self.rect.x, self.rect.y);
        let right = self.rect.x + self.rect.width - 1;
        let bottom = self.rect.y + self.rect.height - 1;

        for x in left..=right {
            self.border.push(frame.pixel(x, top)?);
            self.border.push(frame.pixel(x, bottom)?);
        }
        // The corners are already covered by the horizontal runs.
        for y in (top + 1)..bottom {
            self.border.push(frame.pixel(left, y)?);
            self.border.push(frame.pixel(right, y)?);
        }
        if self.border.is_empty() {
            return None;
        }

        self.border.sort_unstable();

        // Spread is measured between percentiles rather than between the
        // extremes.
        //
        // A logo rectangle found automatically is not the tight box an
        // operator would draw around the mark; it is the bounding box of
        // everything steady near it, and on a real broadcast that can be a
        // couple of hundred pixels a side. Judging such a border by its
        // minimum and maximum means one bright pixel — the corner of a moving
        // object, a caption edge, a compression artefact — disqualifies an
        // otherwise perfectly flat frame. On a busy programme that rejects
        // every frame, and the logo is never learned.
        //
        // The tenth and ninetieth percentiles tolerate that while still
        // requiring the border to be genuinely one colour.
        let (low, high) = if self.strict {
            (self.border.first().copied()?, self.border.last().copied()?)
        } else {
            (
                percentile(&self.border, 0.10)?,
                percentile(&self.border, 0.90)?,
            )
        };
        if high.saturating_sub(low) > self.flatness_threshold {
            return None;
        }

        Some(interquartile_mean(&self.border))
    }

    /// Solve the accumulated statistics into a logo.
    ///
    /// Returns `None` when too few frames were accepted, when the backgrounds
    /// they carried were too alike to identify a line, or when the resulting
    /// fit does not look like a logo.
    #[must_use]
    pub fn finish(
        &self,
        name: String,
        channel_id: Option<String>,
        frame_size: (u32, u32),
    ) -> Option<LogoData> {
        self.fit(name, channel_id, frame_size, true)
    }

    /// Solve without insisting the result looks like a real logo.
    ///
    /// A fit taken from every flat frame is diluted by the ones that carried
    /// no logo — the fades inside the commercials — and on a recording with
    /// many of those it can fall below the plausibility bar entirely. It is
    /// still good enough to tell which frames had the logo, which is all the
    /// refinement pass needs it for. The refined fit is held to the full bar.
    #[must_use]
    pub fn finish_bootstrap(
        &self,
        name: String,
        channel_id: Option<String>,
        frame_size: (u32, u32),
    ) -> Option<LogoData> {
        self.fit(name, channel_id, frame_size, false)
    }

    fn fit(
        &self,
        name: String,
        channel_id: Option<String>,
        frame_size: (u32, u32),
        require_plausible: bool,
    ) -> Option<LogoData> {
        if self.frames < MINIMUM_FRAMES {
            tracing::debug!(
                frames = self.frames,
                "not enough flat-background frames to fit a logo"
            );
            return None;
        }
        let spread = self
            .brightest_background
            .saturating_sub(self.darkest_background);
        if spread < MINIMUM_BACKGROUND_SPREAD {
            tracing::debug!(spread, "background levels too alike to identify a fit");
            return None;
        }

        let n = f64::from(self.frames);
        let mut a = Vec::with_capacity(self.stats.len());
        let mut b = Vec::with_capacity(self.stats.len());

        for stats in &self.stats {
            // Fit in both directions and average, which is far steadier than
            // either alone when the noise is comparable on both axes — as it
            // is here, since both values come from the same compressed frame.
            let forward = fit_line(n, stats.sum_f, stats.sum_b, stats.sum_f2, stats.sum_fb);
            let reverse = fit_line(n, stats.sum_b, stats.sum_f, stats.sum_b2, stats.sum_fb);

            let (slope, intercept) = match (forward, reverse) {
                (Some((a1, b1)), Some((a2, b2))) if a2.abs() > 1e-9 => {
                    (f64::midpoint(a1, 1.0 / a2), f64::midpoint(b1, -b2 / a2))
                }
                // One direction failing is normal for a pixel the logo does
                // not cover; the other still describes it.
                (Some(pair), _) | (_, Some(pair)) => pair,
                (None, None) => (1.0, 0.0),
            };

            if slope.is_finite() && intercept.is_finite() && slope.abs() > 1e-6 {
                a.push(slope as f32);
                b.push(intercept as f32);
            } else {
                // A pixel with no usable fit is treated as logo-free rather
                // than poisoning the map with a NaN.
                a.push(1.0);
                b.push(0.0);
            }
        }

        let mut logo = LogoData {
            name,
            channel_id,
            source_width: frame_size.0,
            source_height: frame_size.1,
            rect: self.rect,
            a,
            b,
            frames_used: self.frames,
        };

        // Noise can fit a slope the compositing model cannot produce. Those
        // pixels must be neutralised here rather than at each use, or a
        // negative slope would invert the picture in `remove` and manufacture
        // an edge for the detector to lock onto.
        logo.canonicalise();

        if require_plausible && !logo.is_plausible() {
            return None;
        }
        Some(logo)
    }
}

/// Value at a fraction of the way through a sorted slice.
fn percentile(sorted: &[u8], fraction: f32) -> Option<u8> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() - 1) as f32 * fraction).round() as usize;
    sorted.get(index.min(sorted.len() - 1)).copied()
}

/// Mean of the middle half of a sorted slice.
///
/// Trimming the quartiles discards the ringing at the extremes of a
/// compressed flat area without the instability of a bare median.
fn interquartile_mean(sorted: &[u8]) -> u8 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    let skip = n / 4;
    let slice = sorted.get(skip..n - skip).unwrap_or(sorted);
    if slice.is_empty() {
        return sorted.get(n / 2).copied().unwrap_or(0);
    }
    let count = slice.len() as u32;
    let total: u32 = slice.iter().map(|&v| u32::from(v)).sum();
    // Round to nearest rather than flooring: a consistent half-level bias in
    // the background estimate shifts the fitted intercept for every pixel.
    ((total + count / 2) / count).min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    const FRAME_W: u32 = 64;
    const FRAME_H: u32 = 48;

    fn rect() -> Rect {
        Rect {
            x: 8,
            y: 8,
            width: 16,
            height: 12,
        }
    }

    /// Composite a known logo over a flat background and return the frame.
    ///
    /// The logo occupies the interior of the rectangle; its border is left
    /// clean, which is what the scanner requires.
    fn synthetic_frame(background: u8, alpha: f32, colour: f32, noise: i32) -> Vec<u8> {
        let mut luma = vec![background; (FRAME_W * FRAME_H) as usize];
        let r = rect();
        for row in 1..r.height - 1 {
            for column in 1..r.width - 1 {
                let index = ((r.y + row) * FRAME_W + r.x + column) as usize;
                let composited = (1.0 - alpha) * f32::from(background) / 255.0 + alpha * colour;
                luma[index] = (composited * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        if noise != 0 {
            for (index, value) in luma.iter_mut().enumerate() {
                // Deterministic dither, so the test does not depend on an RNG.
                let wobble =
                    i32::try_from(index % 4096).unwrap_or(0) * 7919 % (2 * noise + 1) - noise;
                *value = (i32::from(*value) + wobble).clamp(0, 255) as u8;
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

    /// Feed a spread of backgrounds, as a real programme's fades would.
    fn scan_with(alpha: f32, colour: f32, noise: i32, backgrounds: &[u8]) -> LogoScanner {
        let mut scanner = LogoScanner::new(rect(), DEFAULT_FLATNESS_THRESHOLD);
        // Enough rounds that even a single-background run clears
        // `MINIMUM_FRAMES`, so the spread check is what a test exercises
        // rather than the frame count incidentally tripping first.
        for round in 0..60 {
            for &background in backgrounds {
                let shifted = background.saturating_add((round % 3) as u8);
                let luma = synthetic_frame(shifted, alpha, colour, noise);
                scanner.add_frame(&frame(&luma));
            }
        }
        scanner
    }

    #[test]
    fn recovers_a_known_logo_from_clean_frames() {
        let (alpha, colour) = (0.5f32, 0.9f32);
        let scanner = scan_with(alpha, colour, 0, &[10, 60, 120, 200]);
        assert!(scanner.frames_accepted() >= MINIMUM_FRAMES);

        let logo = scanner
            .finish("test".into(), None, (FRAME_W, FRAME_H))
            .expect("a plausible logo");

        // A pixel in the middle of the logo.
        let r = rect();
        let index = ((r.height / 2) * r.width + r.width / 2) as usize;
        assert_relative_eq!(logo.alpha_at(index), alpha, epsilon = 0.03);
        assert_relative_eq!(logo.colour_at(index), colour, epsilon = 0.05);
    }

    #[test]
    fn recovers_a_logo_despite_compression_style_noise() {
        let (alpha, colour) = (0.45f32, 0.85f32);
        let scanner = scan_with(alpha, colour, 3, &[10, 60, 120, 200]);
        let logo = scanner
            .finish("noisy".into(), None, (FRAME_W, FRAME_H))
            .expect("a plausible logo");

        let r = rect();
        let index = ((r.height / 2) * r.width + r.width / 2) as usize;
        assert_relative_eq!(logo.alpha_at(index), alpha, epsilon = 0.08);
    }

    #[test]
    fn leaves_pixels_outside_the_logo_transparent() {
        let scanner = scan_with(0.5, 0.9, 0, &[10, 60, 120, 200]);
        let logo = scanner
            .finish("test".into(), None, (FRAME_W, FRAME_H))
            .expect("a plausible logo");

        // The rectangle's own border is background, never logo.
        assert_relative_eq!(logo.alpha_at(0), 0.0, epsilon = 0.02);
    }

    #[test]
    fn a_few_stray_pixels_do_not_disqualify_a_flat_border() {
        // A large auto-located rectangle's border crosses a lot of picture,
        // and one bright intruder — the corner of a moving object, a caption
        // edge — must not reject the frame. On a busy programme, judging by
        // the extremes rejects every frame and the logo is never learned.
        let mut scanner = LogoScanner::new(rect(), DEFAULT_FLATNESS_THRESHOLD);
        let mut luma = synthetic_frame(80, 0.5, 0.9, 0);

        let r = rect();
        for offset in 0..3u32 {
            let index = ((r.y * FRAME_W) + r.x + offset) as usize;
            luma[index] = 255;
        }

        assert!(
            scanner.add_frame(&frame(&luma)),
            "a handful of outliers must not reject an otherwise flat border"
        );
    }

    #[test]
    fn a_genuinely_varied_border_is_still_rejected() {
        // Tolerating outliers must not turn into tolerating a gradient.
        let mut scanner = LogoScanner::new(rect(), DEFAULT_FLATNESS_THRESHOLD);
        let mut luma = synthetic_frame(80, 0.5, 0.9, 0);

        let r = rect();
        for offset in 0..r.width {
            let index = ((r.y * FRAME_W) + r.x + offset) as usize;
            luma[index] = (offset * 8).min(255) as u8;
        }

        assert!(!scanner.add_frame(&frame(&luma)));
    }

    #[test]
    fn rejects_frames_whose_background_is_not_flat() {
        let mut scanner = LogoScanner::new(rect(), DEFAULT_FLATNESS_THRESHOLD);
        // A hard gradient across the frame: no single background colour.
        let mut luma = vec![0u8; (FRAME_W * FRAME_H) as usize];
        for (index, value) in luma.iter_mut().enumerate() {
            *value = ((index as u32 % FRAME_W) * 4).min(255) as u8;
        }
        assert!(!scanner.add_frame(&frame(&luma)));
        assert_eq!(scanner.frames_accepted(), 0);
    }

    #[test]
    fn refuses_to_fit_when_every_background_was_the_same() {
        // A programme that only ever fades to black leaves the slope and the
        // intercept indistinguishable.
        let scanner = scan_with(0.5, 0.9, 0, &[10]);
        assert!(scanner.frames_accepted() >= MINIMUM_FRAMES);
        assert!(
            scanner
                .finish("flat".into(), None, (FRAME_W, FRAME_H))
                .is_none(),
            "a single background level must not yield a logo"
        );
    }

    #[test]
    fn refuses_to_fit_from_too_few_frames() {
        let mut scanner = LogoScanner::new(rect(), DEFAULT_FLATNESS_THRESHOLD);
        for background in [10u8, 60, 120] {
            let luma = synthetic_frame(background, 0.5, 0.9, 0);
            scanner.add_frame(&frame(&luma));
        }
        assert!(
            scanner
                .finish("few".into(), None, (FRAME_W, FRAME_H))
                .is_none()
        );
    }

    #[test]
    fn reports_no_logo_when_the_flat_frames_carried_none() {
        let scanner = scan_with(0.0, 0.0, 0, &[10, 60, 120, 200]);
        assert!(
            scanner
                .finish("empty".into(), None, (FRAME_W, FRAME_H))
                .is_none(),
            "flat frames without a logo must not produce one"
        );
    }

    #[test]
    fn refitting_from_logo_present_frames_only_recovers_the_true_opacity() {
        use crate::logo::detect::LogoDetector;

        let (alpha, colour) = (0.5f32, 0.9f32);
        let backgrounds = [10u8, 60, 120, 200];

        // A real recording's flat frames are a mixture: the programme fades
        // to black carrying its logo, and so do the commercials, which do not.
        // Three frames in five here carry the logo.
        let mut frames: Vec<Vec<u8>> = Vec::new();
        for round in 0..60u32 {
            for &background in &backgrounds {
                let shifted = background.saturating_add((round % 3) as u8);
                let present = round % 5 < 3;
                let logo_alpha = if present { alpha } else { 0.0 };
                frames.push(synthetic_frame(shifted, logo_alpha, colour, 0));
            }
        }

        let r = rect();
        let centre = ((r.height / 2) * r.width + r.width / 2) as usize;

        // Fitting from everything mixes two different relationships — the
        // compositing line, and plain "observed equals background" — and drags
        // the estimate toward zero.
        let mut bootstrap = LogoScanner::new(r, DEFAULT_FLATNESS_THRESHOLD);
        for luma in &frames {
            bootstrap.add_frame(&frame(luma));
        }
        let bootstrap = bootstrap
            .finish_bootstrap("bootstrap".into(), None, (FRAME_W, FRAME_H))
            .expect("a bootstrap fit");
        let contaminated = bootstrap.alpha_at(centre);
        assert!(
            contaminated < alpha - 0.08,
            "the mixed fit should understate opacity, got {contaminated} against {alpha}"
        );

        // Refitting from only the frames the bootstrap recognises recovers it.
        let mut gate = LogoDetector::new(bootstrap).expect("a usable bootstrap detector");
        let mut refined = LogoScanner::new(r, DEFAULT_FLATNESS_THRESHOLD);
        for luma in &frames {
            let candidate = frame(luma);
            if gate.score(&candidate) >= 0.25 {
                refined.add_frame(&candidate);
            }
        }
        let refined = refined
            .finish("refined".into(), None, (FRAME_W, FRAME_H))
            .expect("a refined fit");

        let recovered = refined.alpha_at(centre);
        assert!(
            (recovered - alpha).abs() < 0.08,
            "refined opacity {recovered} should be close to {alpha} (bootstrap gave {contaminated})"
        );
        assert!(
            recovered > contaminated,
            "refinement must improve on the bootstrap"
        );
    }

    #[test]
    fn interquartile_mean_ignores_the_extremes() {
        // Sorted input with outliers at both ends.
        let values = [0u8, 100, 100, 100, 100, 100, 100, 255];
        assert_eq!(interquartile_mean(&values), 100);
    }

    #[test]
    fn line_fit_rejects_a_degenerate_set() {
        // Every x identical: no slope is determined.
        let n = 10.0;
        let sum_x = 10.0 * 5.0;
        let sum_x2 = 10.0 * 25.0;
        assert!(fit_line(n, sum_x, 30.0, sum_x2, 150.0).is_none());
    }

    #[test]
    fn line_fit_recovers_a_known_line() {
        // y = 2x + 1 through x = 0..4
        let points: Vec<(f64, f64)> = (0..5)
            .map(|x| (f64::from(x), 2.0 * f64::from(x) + 1.0))
            .collect();
        let n = points.len() as f64;
        let sum_x: f64 = points.iter().map(|p| p.0).sum();
        let sum_y: f64 = points.iter().map(|p| p.1).sum();
        let sum_squares: f64 = points.iter().map(|p| p.0 * p.0).sum();
        let sum_products: f64 = points.iter().map(|p| p.0 * p.1).sum();

        let (slope, intercept) =
            fit_line(n, sum_x, sum_y, sum_squares, sum_products).expect("a line");
        assert_relative_eq!(slope, 2.0, epsilon = 1e-9);
        assert_relative_eq!(intercept, 1.0, epsilon = 1e-9);
    }
}
