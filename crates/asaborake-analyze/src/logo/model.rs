//! The learned logo itself: what it is, and how it is stored.
//!
//! # The model
//!
//! A station logo is not an opaque stamp. It composites over the picture with
//! a per-pixel opacity, so an observed pixel is
//!
//! ```text
//! observed = (1 - alpha) * background + alpha * colour
//! ```
//!
//! Asaborake stores the *inverse* of that relation, because removing a logo
//! and scoring a frame against one both need to go from observed back to
//! background:
//!
//! ```text
//! background = a * observed + b
//! ```
//!
//! with `a = 1 / (1 - alpha)` and `b = -alpha * colour / (1 - alpha)`. Storing
//! `(a, b)` rather than `(alpha, colour)` keeps the hot loop to one multiply
//! and one add per pixel, and makes the fit a plain linear regression.
//!
//! All pixel values here are normalised to `0.0..=1.0`.
//!
//! This is the model Amatsukaze's `LogoScan.hpp` uses; see `ATTRIBUTION.md`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Magic bytes at the head of an `.abl` file.
const MAGIC: &[u8; 4] = b"ABL1";

/// Opacity at which a pixel counts as part of the logo rather than noise.
///
/// Lowering this to 0.10 was tried, to admit the faint tv asahi watermark
/// whose fit peaks at 0.17. It was put back: the fit that came in under the
/// looser bar was a regular repeating pattern rather than the eight varied
/// letters of the logo, so the bar was not what was keeping it out — the
/// estimator was returning the wrong thing, and accepting it would have
/// stored a bad logo and reused it on every recording from that channel.
///
/// The scan reports its numbers now, so a fit rejected here can be looked at
/// rather than guessed about.
pub const STRONG_ALPHA: f32 = 0.20;

/// How many such pixels a real logo has.
///
/// A logo is a few hundred solid pixels in a mostly empty rectangle, so this
/// sits far below what one produces and far above what noise does.
pub const MINIMUM_STRONG_PIXELS: usize = 20;

/// A rectangle within a frame, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rect {
    /// Number of pixels the rectangle covers.
    #[must_use]
    pub const fn area(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// Whether the rectangle fits entirely inside a frame of this size.
    ///
    /// The addition is checked: a logo file is untrusted input, and an `x`
    /// near `u32::MAX` would otherwise wrap and report a rectangle far outside
    /// the frame as fitting inside it.
    #[must_use]
    pub const fn fits_within(&self, width: u32, height: u32) -> bool {
        let (Some(right), Some(bottom)) = (
            self.x.checked_add(self.width),
            self.y.checked_add(self.height),
        ) else {
            return false;
        };
        right <= width && bottom <= height
    }

    /// Whether the rectangle has a non-zero area and does not overflow.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.x.checked_add(self.width).is_some()
            && self.y.checked_add(self.height).is_some()
    }

    /// Grow by `margin` on every side, clamped to the frame.
    #[must_use]
    pub fn expanded(&self, margin: u32, width: u32, height: u32) -> Self {
        let x = self.x.saturating_sub(margin);
        let y = self.y.saturating_sub(margin);
        Self {
            x,
            y,
            width: (self.x + self.width + margin).min(width).saturating_sub(x),
            height: (self.y + self.height + margin)
                .min(height)
                .saturating_sub(y),
        }
    }
}

/// A learned logo: per-pixel removal coefficients over a rectangle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogoData {
    /// Human-readable name, usually the channel name.
    pub name: String,
    /// Channel this logo belongs to, as `EPGStation` reports it.
    pub channel_id: Option<String>,
    /// Width of the frames it was learned from.
    pub source_width: u32,
    /// Height of the frames it was learned from.
    pub source_height: u32,
    /// Where in the frame the logo sits.
    pub rect: Rect,
    /// Slope of the per-pixel regression, row-major over `rect`.
    pub a: Vec<f32>,
    /// Intercept of the per-pixel regression, row-major over `rect`.
    pub b: Vec<f32>,
    /// How many frames contributed to the fit.
    pub frames_used: u32,
}

impl LogoData {
    /// Recover the opacity at a pixel, in `0.0..=1.0`.
    ///
    /// `a` below 1 would mean a negative opacity, which the fit can produce
    /// from noise; those pixels are reported as fully transparent.
    #[must_use]
    pub fn alpha_at(&self, index: usize) -> f32 {
        let Some(&a) = self.a.get(index) else {
            return 0.0;
        };
        if a <= 1.0 || !a.is_finite() {
            return 0.0;
        }
        (1.0 - 1.0 / a).clamp(0.0, 1.0)
    }

    /// Recover the logo's own colour at a pixel, in `0.0..=1.0`.
    #[must_use]
    pub fn colour_at(&self, index: usize) -> f32 {
        let (Some(&a), Some(&b)) = (self.a.get(index), self.b.get(index)) else {
            return 0.0;
        };
        // colour = -b / (a - 1); a at or below 1 means no logo here.
        if a <= 1.000_01 || !a.is_finite() || !b.is_finite() {
            return 0.0;
        }
        (-b / (a - 1.0)).clamp(0.0, 1.0)
    }

    /// Whether this logo was learned from frames of the given size.
    ///
    /// A logo is only meaningful at the resolution it was learned at: the
    /// rectangle is in pixels, so reusing a 1920x1080 logo on 1440x1080
    /// broadcast samples the wrong part of the picture entirely and reports
    /// the logo absent for the whole recording.
    #[must_use]
    pub const fn matches_frame_size(&self, width: u32, height: u32) -> bool {
        self.source_width == width && self.source_height == height
    }

    /// Remove the logo from an observed value, recovering the background.
    #[must_use]
    pub fn remove(&self, index: usize, observed: f32) -> f32 {
        let (Some(&a), Some(&b)) = (self.a.get(index), self.b.get(index)) else {
            return observed;
        };
        a.mul_add(observed, b)
    }

    /// Force every coefficient to describe a physically possible logo.
    ///
    /// The model implies `a = 1/(1-alpha)` with `alpha` in `0..1`, so `a` is
    /// always at least 1. Noise in the fit can produce values below that, and
    /// even negative ones; left alone they still transform pixels in `remove`
    /// and `apply`, where a negative slope inverts the picture and manufactures
    /// a strong synthetic edge that then dominates feature selection.
    ///
    /// Anything outside the physical range is rewritten as "no logo here".
    pub fn canonicalise(&mut self) {
        for (a, b) in self.a.iter_mut().zip(self.b.iter_mut()) {
            if !a.is_finite() || !b.is_finite() || *a < 1.0 {
                *a = 1.0;
                *b = 0.0;
            }
        }
    }

    /// Composite the logo onto a background value.
    ///
    /// This is `remove` inverted, and it is how the synthetic reference images
    /// the detector correlates against are built.
    #[must_use]
    pub fn apply(&self, index: usize, background: f32) -> f32 {
        let (Some(&a), Some(&b)) = (self.a.get(index), self.b.get(index)) else {
            return background;
        };
        if a.abs() < f32::EPSILON || !a.is_finite() {
            return background;
        }
        (background - b) / a
    }

    /// Mean opacity across the rectangle, a rough measure of how much logo
    /// there is to find.
    #[must_use]
    pub fn mean_alpha(&self) -> f32 {
        if self.a.is_empty() {
            return 0.0;
        }
        let total: f32 = (0..self.a.len()).map(|i| self.alpha_at(i)).sum();
        total / self.a.len() as f32
    }

    /// How many pixels came out opaque enough to match against.
    ///
    /// A logo is a few hundred solid pixels in a mostly empty rectangle. A fit
    /// spread thinly over the whole box, with none of it strong, is noise that
    /// happens to have a slope.
    #[must_use]
    pub fn strong_pixels(&self) -> usize {
        (0..self.a.len())
            .filter(|&i| self.alpha_at(i) >= STRONG_ALPHA)
            .count()
    }

    /// Whether the fit produced something that looks like a real logo.
    ///
    /// A fit over frames that happened to be flat but carried no logo yields
    /// near-zero opacity everywhere; encoding that as a logo would make the
    /// detector report noise for the whole recording.
    #[must_use]
    pub fn is_plausible(&self) -> bool {
        const MINIMUM_FRAMES: u32 = 50;

        if self.frames_used < MINIMUM_FRAMES || self.a.is_empty() {
            return false;
        }
        self.strong_pixels() >= MINIMUM_STRONG_PIXELS
    }

    /// Render the logo as an RGBA image for the web UI.
    ///
    /// The alpha channel is the learned opacity and the colour channel the
    /// learned colour, so what a reviewer sees is exactly what the detector
    /// matches against.
    #[must_use]
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rect.area() * 4);
        for index in 0..self.rect.area() {
            let luma = (self.colour_at(index) * 255.0).round().clamp(0.0, 255.0) as u8;
            let alpha = (self.alpha_at(index) * 255.0).round().clamp(0.0, 255.0) as u8;
            out.extend_from_slice(&[luma, luma, luma, alpha]);
        }
        out
    }

    /// Write a PNG preview of the logo.
    ///
    /// # Errors
    /// Returns [`Error::Image`] if encoding fails.
    pub fn write_png(&self, path: &Path) -> Result<(), Error> {
        let buffer = image::RgbaImage::from_raw(self.rect.width, self.rect.height, self.to_rgba())
            .ok_or(Error::LogoGeometry)?;
        buffer.save(path).map_err(|source| Error::Image {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
    }

    /// Serialise to Asaborake's compact `.abl` form.
    ///
    /// # Errors
    /// Returns [`Error::LogoEncode`] if serialisation fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::from(MAGIC);
        let body = postcard::to_stdvec(self).map_err(Error::LogoEncode)?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Read the `.abl` form.
    ///
    /// # Errors
    /// Returns [`Error::LogoFormat`] if the magic bytes are wrong, or
    /// [`Error::LogoDecode`] if the body is malformed.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let body = bytes.strip_prefix(MAGIC).ok_or(Error::LogoFormat)?;
        let mut logo: Self = postcard::from_bytes(body).map_err(Error::LogoDecode)?;

        // A logo file may have been produced by another version, copied from a
        // logo pack, or simply be corrupt. Every downstream stage indexes by
        // the rectangle and does arithmetic on the coefficients, so both are
        // checked here rather than trusted.
        if !logo.rect.is_valid()
            || !logo.rect.fits_within(logo.source_width, logo.source_height)
            || logo.a.len() != logo.rect.area()
            || logo.b.len() != logo.rect.area()
        {
            return Err(Error::LogoGeometry);
        }

        // Non-finite coefficients would propagate a NaN into every frame score
        // and silently disable detection for the whole recording.
        logo.canonicalise();
        Ok(logo)
    }

    /// Save to a file in `.abl` form.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        std::fs::write(path, self.to_bytes()?).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load from a `.abl` file.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file cannot be read.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let bytes = std::fs::read(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_bytes(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// Build a uniform logo of the given opacity and colour.
    fn uniform_logo(alpha: f32, colour: f32, width: u32, height: u32) -> LogoData {
        let count = (width * height) as usize;
        let a = 1.0 / (1.0 - alpha);
        let b = -alpha * colour / (1.0 - alpha);
        LogoData {
            name: "test".into(),
            channel_id: None,
            source_width: width * 4,
            source_height: height * 4,
            rect: Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            a: vec![a; count],
            b: vec![b; count],
            frames_used: 200,
        }
    }

    #[test]
    fn recovers_the_opacity_and_colour_it_was_built_from() {
        let logo = uniform_logo(0.4, 0.9, 8, 8);
        assert_relative_eq!(logo.alpha_at(0), 0.4, epsilon = 1e-5);
        assert_relative_eq!(logo.colour_at(0), 0.9, epsilon = 1e-5);
        assert_relative_eq!(logo.mean_alpha(), 0.4, epsilon = 1e-5);
    }

    #[test]
    fn removing_a_composited_logo_returns_the_background() {
        let (alpha, colour) = (0.35f32, 0.8f32);
        let logo = uniform_logo(alpha, colour, 4, 4);

        for background in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let observed = (1.0 - alpha) * background + alpha * colour;
            assert_relative_eq!(logo.remove(0, observed), background, epsilon = 1e-5);
            // And compositing is the exact inverse.
            assert_relative_eq!(logo.apply(0, background), observed, epsilon = 1e-5);
        }
    }

    #[test]
    fn a_fully_transparent_fit_reports_no_logo() {
        let mut logo = uniform_logo(0.0, 0.0, 4, 4);
        assert_relative_eq!(logo.alpha_at(0), 0.0);
        assert!(!logo.is_plausible(), "zero opacity is not a logo");

        // Nor is a fit whose slope came out below one, which noise can do.
        logo.a = vec![0.5; logo.rect.area()];
        assert_relative_eq!(logo.alpha_at(0), 0.0);
    }

    #[test]
    fn a_fit_from_too_few_frames_is_not_trusted() {
        let mut logo = uniform_logo(0.5, 1.0, 8, 8);
        assert!(logo.is_plausible());
        logo.frames_used = 10;
        assert!(!logo.is_plausible(), "10 frames is not enough to trust");
    }

    #[test]
    fn round_trips_through_the_abl_form() {
        let logo = uniform_logo(0.45, 0.7, 6, 5);
        let bytes = logo.to_bytes().expect("encodes");
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(LogoData::from_bytes(&bytes).expect("decodes"), logo);
    }

    #[test]
    fn rejects_a_file_that_is_not_a_logo() {
        assert!(matches!(
            LogoData::from_bytes(b"not a logo at all"),
            Err(Error::LogoFormat)
        ));
    }

    #[test]
    fn rejects_a_logo_whose_arrays_do_not_match_its_rectangle() {
        let mut logo = uniform_logo(0.4, 0.9, 4, 4);
        logo.a.truncate(3);
        let bytes = logo.to_bytes().expect("encodes");
        assert!(matches!(
            LogoData::from_bytes(&bytes),
            Err(Error::LogoGeometry)
        ));
    }

    #[test]
    fn nonphysical_slopes_are_rewritten_as_no_logo() {
        let mut logo = uniform_logo(0.4, 0.9, 4, 4);
        // Noise in the fit can produce a slope below one, or a negative one;
        // left alone, `remove` would still transform — and a negative slope
        // inverts the picture, manufacturing an edge that is not there.
        logo.a[0] = -3.0;
        logo.b[0] = 0.2;
        logo.a[1] = 0.5;
        logo.a[2] = f32::NAN;

        logo.canonicalise();

        for index in 0..3 {
            assert_relative_eq!(logo.a[index], 1.0);
            assert_relative_eq!(logo.b[index], 0.0);
            // And the pixel now passes through untouched.
            assert_relative_eq!(logo.remove(index, 0.42), 0.42);
            assert_relative_eq!(logo.alpha_at(index), 0.0);
        }
        // The healthy pixels are left alone.
        assert_relative_eq!(logo.alpha_at(3), 0.4, epsilon = 1e-5);
    }

    #[test]
    fn a_logo_is_only_valid_at_the_resolution_it_was_learned_at() {
        let logo = uniform_logo(0.4, 0.9, 8, 8);
        assert!(logo.matches_frame_size(32, 32));
        // Reusing a 1920x1080 logo on 1440x1080 broadcast would sample the
        // wrong pixels entirely.
        assert!(!logo.matches_frame_size(1440, 1080));
    }

    #[test]
    fn a_rectangle_that_would_overflow_does_not_report_as_fitting() {
        let rect = Rect {
            x: u32::MAX - 1,
            y: 0,
            width: 100,
            height: 10,
        };
        assert!(!rect.fits_within(u32::MAX, u32::MAX));
        assert!(!rect.is_valid());

        let empty = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        };
        assert!(!empty.is_valid());
    }

    #[test]
    fn a_logo_whose_rectangle_escapes_its_source_frame_is_rejected() {
        let mut logo = uniform_logo(0.4, 0.9, 4, 4);
        logo.source_width = 2;
        logo.source_height = 2;
        let bytes = logo.to_bytes().expect("encodes");
        assert!(matches!(
            LogoData::from_bytes(&bytes),
            Err(Error::LogoGeometry)
        ));
    }

    #[test]
    fn loading_scrubs_non_finite_coefficients() {
        let mut logo = uniform_logo(0.4, 0.9, 4, 4);
        logo.a[5] = f32::INFINITY;
        logo.b[5] = f32::NAN;

        let bytes = logo.to_bytes().expect("encodes");
        let loaded = LogoData::from_bytes(&bytes).expect("decodes");

        assert!(loaded.a.iter().all(|v| v.is_finite()));
        assert!(loaded.b.iter().all(|v| v.is_finite()));
        assert_relative_eq!(loaded.alpha_at(5), 0.0);
    }

    #[test]
    fn preview_has_one_rgba_quad_per_pixel() {
        let logo = uniform_logo(0.5, 1.0, 3, 2);
        let rgba = logo.to_rgba();
        assert_eq!(rgba.len(), 3 * 2 * 4);
        assert_eq!(rgba[3], 128, "alpha 0.5 renders as mid opacity");
        assert_eq!(rgba[0], 255, "colour 1.0 renders as white");
    }

    #[test]
    fn rectangles_expand_without_leaving_the_frame() {
        let rect = Rect {
            x: 5,
            y: 5,
            width: 10,
            height: 10,
        };
        let grown = rect.expanded(3, 100, 100);
        assert_eq!(
            grown,
            Rect {
                x: 2,
                y: 2,
                width: 16,
                height: 16
            }
        );

        let corner = Rect {
            x: 1,
            y: 1,
            width: 4,
            height: 4,
        };
        let clamped = corner.expanded(10, 8, 8);
        assert_eq!(clamped.x, 0);
        assert_eq!(clamped.y, 0);
        assert!(clamped.fits_within(8, 8), "{clamped:?}");
    }
}
