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
            border: Vec::new(),
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

    /// Offer a frame; returns whether its background was flat enough to use.
    pub fn add_frame(&mut self, frame: &Frame<'_>) -> bool {
        if !self.rect.fits_within(frame.width, frame.height) {
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
        let lowest = *self.border.first()?;
        let highest = *self.border.last()?;
        if highest.saturating_sub(lowest) > self.flatness_threshold {
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

        let logo = LogoData {
            name,
            channel_id,
            source_width: frame_size.0,
            source_height: frame_size.1,
            rect: self.rect,
            a,
            b,
            frames_used: self.frames,
        };

        logo.is_plausible().then_some(logo)
    }
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
    let total: u32 = slice.iter().map(|&v| u32::from(v)).sum();
    (total / slice.len() as u32).min(255) as u8
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
