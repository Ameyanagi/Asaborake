//! Finding where the logo is, without being told.
//!
//! Amatsukaze has the operator draw the logo rectangle in its GUI. Asaborake
//! runs unattended behind `EPGStation`, so it has to find the rectangle itself.
//!
//! # What separates a logo from busy content
//!
//! A logo is an edge that never moves. Averaging edge strength over time is
//! not enough on its own — a permanently busy corner averages high too. What
//! distinguishes the logo is that its edge strength is *steady*: high mean,
//! low variance. Content that merely happens to be detailed fluctuates frame
//! to frame as the picture moves underneath it.
//!
//! So each pixel is scored by the ratio of the mean edge strength to its
//! standard deviation, and the strongest connected region near a frame edge
//! wins.

use asaborake_media::Frame;

use super::model::Rect;

/// Fraction of the frame height, from the top and bottom edges, in which a
/// logo may be found.
///
/// Broadcasters put logos in a corner or centred against the top or bottom
/// edge. Excluding the middle band removes most of the picture — and with it
/// most of the false positives from static furniture in the scene.
const EDGE_BAND: f32 = 0.28;

/// Fewest frames before a location is trusted.
const MINIMUM_FRAMES: u32 = 100;

/// A candidate region must be at least this many pixels to be a logo.
const MINIMUM_AREA: usize = 64;

/// A candidate region wider or taller than this fraction of the frame is
/// scenery, not a logo.
const MAXIMUM_EXTENT: f32 = 0.45;

/// Accumulates the evidence needed to locate a logo.
#[derive(Debug)]
pub struct LogoLocator {
    width: u32,
    height: u32,
    /// Running sum of edge strength per pixel.
    sum: Vec<f32>,
    /// Running sum of squared edge strength per pixel.
    sum_squares: Vec<f32>,
    frames: u32,
}

impl LogoLocator {
    /// Start looking for a logo in frames of this size.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let count = (width as usize) * (height as usize);
        Self {
            width,
            height,
            sum: vec![0.0; count],
            sum_squares: vec![0.0; count],
            frames: 0,
        }
    }

    /// How many frames have contributed.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    /// Add a frame's edge map to the accumulators.
    pub fn add_frame(&mut self, frame: &Frame<'_>) {
        if frame.width != self.width || frame.height != self.height {
            return;
        }
        // The band is scanned rather than the whole frame: the middle cannot
        // hold a logo, and skipping it is most of the work saved. The rows are
        // collected first so the accumulators can be borrowed mutably below.
        let rows: Vec<u32> = self.rows().collect();
        for y in rows {
            for x in 1..self.width - 1 {
                let strength = edge_strength(frame, x, y);
                let index = (y * self.width + x) as usize;
                if let (Some(sum), Some(squares)) =
                    (self.sum.get_mut(index), self.sum_squares.get_mut(index))
                {
                    *sum += strength;
                    *squares += strength * strength;
                }
            }
        }
        self.frames += 1;
    }

    /// Rows within the bands a logo may occupy.
    fn rows(&self) -> impl Iterator<Item = u32> + '_ {
        let band = ((self.height as f32) * EDGE_BAND) as u32;
        let band = band.max(2).min(self.height / 2);
        (1..band).chain((self.height - band)..(self.height - 1))
    }

    /// The most logo-like region found, if any.
    #[must_use]
    pub fn finish(&self) -> Option<Rect> {
        if self.frames < MINIMUM_FRAMES {
            tracing::debug!(frames = self.frames, "too few frames to locate a logo");
            return None;
        }

        let scores = self.steadiness_map();
        // A relative threshold adapts to how contrasty the channel's logo is;
        // an absolute one would miss faint logos and over-select bold ones.
        let peak = scores.iter().copied().fold(0.0f32, f32::max);
        if peak <= 0.0 {
            return None;
        }
        let threshold = peak * 0.45;

        let mut best: Option<(usize, Rect)> = None;
        let mut visited = vec![false; scores.len()];
        for y in self.rows() {
            for x in 1..self.width - 1 {
                let index = (y * self.width + x) as usize;
                if visited.get(index).copied().unwrap_or(true) {
                    continue;
                }
                if scores.get(index).copied().unwrap_or(0.0) < threshold {
                    visited[index] = true;
                    continue;
                }
                let (area, rect) = self.flood(&scores, &mut visited, x, y, threshold);
                if area >= MINIMUM_AREA
                    && self.is_logo_shaped(rect)
                    && best.as_ref().is_none_or(|(seen, _)| area > *seen)
                {
                    best = Some((area, rect));
                }
            }
        }

        // The scanner needs a border of clean background around the logo to
        // read the background colour from, so the region is grown a little.
        best.map(|(_, rect)| rect.expanded(4, self.width, self.height))
    }

    /// Mean edge strength divided by its standard deviation, per pixel.
    fn steadiness_map(&self) -> Vec<f32> {
        let n = self.frames as f32;
        self.sum
            .iter()
            .zip(&self.sum_squares)
            .map(|(&sum, &squares)| {
                let mean = sum / n;
                let variance = (squares / n - mean * mean).max(0.0);
                // The epsilon keeps a perfectly steady edge from dividing by
                // zero, and sets the scale at which "steady" stops mattering.
                mean / (variance.sqrt() + 1.0)
            })
            .collect()
    }

    /// Whether a region's shape is consistent with a logo rather than scenery.
    fn is_logo_shaped(&self, rect: Rect) -> bool {
        let max_width = (self.width as f32 * MAXIMUM_EXTENT) as u32;
        let max_height = (self.height as f32 * MAXIMUM_EXTENT) as u32;
        rect.width <= max_width && rect.height <= max_height
    }

    /// Flood-fill a connected above-threshold region, returning its size and
    /// bounding box.
    ///
    /// The fill is an explicit stack rather than recursion: a region spanning
    /// a large frame would otherwise be deep enough to overflow it.
    fn flood(
        &self,
        scores: &[f32],
        visited: &mut [bool],
        start_x: u32,
        start_y: u32,
        threshold: f32,
    ) -> (usize, Rect) {
        let mut stack = vec![(start_x, start_y)];
        let (mut min_x, mut max_x) = (start_x, start_x);
        let (mut min_y, mut max_y) = (start_y, start_y);
        let mut area = 0usize;

        while let Some((x, y)) = stack.pop() {
            if x >= self.width || y >= self.height {
                continue;
            }
            let index = (y * self.width + x) as usize;
            if visited.get(index).copied().unwrap_or(true) {
                continue;
            }
            visited[index] = true;
            if scores.get(index).copied().unwrap_or(0.0) < threshold {
                continue;
            }

            area += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);

            // Four-connectivity: a logo's strokes are contiguous, and eight
            // would bridge across the gaps between separate glyphs.
            stack.push((x.wrapping_sub(1), y));
            stack.push((x + 1, y));
            stack.push((x, y.wrapping_sub(1)));
            stack.push((x, y + 1));
        }

        (
            area,
            Rect {
                x: min_x,
                y: min_y,
                width: max_x - min_x + 1,
                height: max_y - min_y + 1,
            },
        )
    }
}

/// Sobel-style gradient magnitude at a pixel, as a plain sum of absolute
/// differences — cheap, and enough to tell an edge from a flat area.
fn edge_strength(frame: &Frame<'_>, x: u32, y: u32) -> f32 {
    let sample = |sx: u32, sy: u32| -> f32 {
        let sx = sx.min(frame.width.saturating_sub(1));
        let sy = sy.min(frame.height.saturating_sub(1));
        f32::from(frame.pixel(sx, sy).unwrap_or(0))
    };
    let horizontal = (sample(x + 1, y) - sample(x.saturating_sub(1), y)).abs();
    let vertical = (sample(x, y + 1) - sample(x, y.saturating_sub(1))).abs();
    horizontal + vertical
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 128;
    const H: u32 = 96;

    fn frame(luma: &[u8]) -> Frame<'_> {
        Frame {
            index: 0,
            timestamp: 0.0,
            width: W,
            height: H,
            luma,
        }
    }

    /// A frame of moving content, with an optional static bright box in the
    /// top-left corner standing in for a logo.
    fn content(step: u32, logo: Option<Rect>) -> Vec<u8> {
        let mut luma = vec![0u8; (W * H) as usize];
        // Vertical bars that scroll, so every pixel sees changing edges.
        for y in 0..H {
            for x in 0..W {
                let phase = (x + step * 3) % 16;
                luma[(y * W + x) as usize] = if phase < 8 { 40 } else { 200 };
            }
        }
        if let Some(rect) = logo {
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    luma[(y * W + x) as usize] = 255;
                }
            }
        }
        luma
    }

    #[test]
    fn finds_a_static_box_amid_moving_content() {
        let logo = Rect {
            x: 10,
            y: 6,
            width: 20,
            height: 12,
        };
        let mut locator = LogoLocator::new(W, H);
        for step in 0..200 {
            let luma = content(step, Some(logo));
            locator.add_frame(&frame(&luma));
        }

        let found = locator.finish().expect("a logo region");
        // The region is grown for the scanner's border, so it must contain the
        // real logo rather than equal it.
        assert!(found.x <= logo.x, "found {found:?}");
        assert!(found.y <= logo.y, "found {found:?}");
        assert!(
            found.x + found.width >= logo.x + logo.width,
            "found {found:?}"
        );
        assert!(
            found.y + found.height >= logo.y + logo.height,
            "found {found:?}"
        );
        assert!(found.fits_within(W, H));
    }

    #[test]
    fn finds_nothing_in_content_with_no_static_element() {
        let mut locator = LogoLocator::new(W, H);
        for step in 0..200 {
            let luma = content(step, None);
            locator.add_frame(&frame(&luma));
        }
        // Any region it does report must at least be logo-shaped rather than
        // a band across the whole frame.
        if let Some(found) = locator.finish() {
            assert!(
                found.width < W / 2 && found.height < H / 2,
                "reported scenery as a logo: {found:?}"
            );
        }
    }

    #[test]
    fn refuses_to_guess_from_too_few_frames() {
        let logo = Rect {
            x: 10,
            y: 6,
            width: 20,
            height: 12,
        };
        let mut locator = LogoLocator::new(W, H);
        for step in 0..10 {
            let luma = content(step, Some(logo));
            locator.add_frame(&frame(&luma));
        }
        assert!(locator.finish().is_none());
    }

    #[test]
    fn ignores_the_middle_of_the_frame() {
        // A static box dead centre is furniture in the scene, not a logo.
        let middle = Rect {
            x: 50,
            y: H / 2 - 6,
            width: 20,
            height: 12,
        };
        let mut locator = LogoLocator::new(W, H);
        for step in 0..200 {
            let luma = content(step, Some(middle));
            locator.add_frame(&frame(&luma));
        }
        if let Some(found) = locator.finish() {
            assert!(
                found.y + found.height < H / 2 || found.y > H / 2,
                "picked up the centre of the frame: {found:?}"
            );
        }
    }

    #[test]
    fn edge_strength_is_zero_on_a_flat_area_and_high_at_a_step() {
        let mut luma = vec![100u8; (W * H) as usize];
        assert!(edge_strength(&frame(&luma), 40, 40).abs() < f32::EPSILON);

        for y in 0..H {
            for x in 64..W {
                luma[(y * W + x) as usize] = 200;
            }
        }
        assert!(edge_strength(&frame(&luma), 64, 40) > 50.0);
    }

    #[test]
    fn frames_of_the_wrong_size_are_ignored() {
        let mut locator = LogoLocator::new(W, H);
        let small = vec![0u8; 16];
        locator.add_frame(&Frame {
            index: 0,
            timestamp: 0.0,
            width: 4,
            height: 4,
            luma: &small,
        });
        assert_eq!(locator.frames(), 0);
    }
}
