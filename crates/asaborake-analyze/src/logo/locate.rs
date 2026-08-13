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
//! That is still not enough, because a recording contains long stretches that
//! are static for reasons of their own: a held title card, a station ident, a
//! test pattern, a slide. Measured across the whole recording, any of those
//! can look steadier than the logo.
//!
//! What a logo has and they do not is *persistence*. It is there through the
//! programme — most of the recording — while a static interlude is there for
//! one stretch and gone. So the recording is divided into chunks, each scored
//! separately, and a pixel counts as logo-like only if it stands out in the
//! majority of them.
//!
//! # Frozen graphics, and why steadiness alone is a trap
//!
//! Steadiness of edge strength, on its own, does not describe a logo — it
//! describes anything opaque that never moves, and scores such a thing
//! *higher* than a real logo. A logo is translucent, so its edges fluctuate as
//! the picture changes underneath; a burned-in graphic's edges do not fluctuate
//! at all, and win.
//!
//! Japanese broadcast makes this concrete and common. During a weather or
//! earthquake warning the picture is inset and a banner runs down one edge with
//! a ticker along the bottom — the L-shaped layout, L字. That banner is opaque,
//! static, high-contrast and present for the whole recording, and by the
//! steadiness measure alone it is the most logo-like thing on screen.
//!
//! The property that separates them is the one the whole logo model rests on:
//! **a logo is translucent**. The picture shows through it, so its pixels vary
//! over time. A frozen graphic's pixels do not. So a pixel must also be *alive*
//! — its luma must actually vary across the recording — before it can be part
//! of a logo. That rules out the banner, the inset border, and any other
//! burned-in furniture, without needing to recognise what they are.

use std::collections::VecDeque;

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

/// A cluster must cover at least this many pixels to be a logo.
const MINIMUM_AREA: usize = 64;

/// A single connected blob must cover at least this many pixels to join a
/// cluster. Lower than [`MINIMUM_AREA`], because one stroke of a character is
/// small on its own and only meaningful alongside the others.
const MINIMUM_BLOB_AREA: usize = 8;

/// A candidate wider than this fraction of the frame is scenery, not a logo.
///
/// Station marks are small. Even a wide one with the channel name spelled out
/// is well under a fifth of the picture, and allowing more lets a static
/// element of the set — a window, a desk, a caption bar — win on area alone.
const MAXIMUM_WIDTH: f32 = 0.22;

/// The same for height. Logos are wider than they are tall far more often than
/// the reverse, so this is tighter.
const MAXIMUM_HEIGHT: f32 = 0.18;

/// Frames per chunk, in the decimated stream the locator is fed.
///
/// At the usual decimation this is roughly twenty seconds of recording: long
/// enough for a chunk's statistics to mean something, short enough that a
/// half-hour programme yields plenty of chunks to vote.
const FRAMES_PER_CHUNK: u32 = 60;

/// Fraction of chunks a pixel must stand out in to be part of a logo.
///
/// A logo is present through the programme, which is the majority of any
/// recording. A static interlude is present for one stretch of it.
const REQUIRED_CHUNK_SHARE: f32 = 0.5;

/// How much the picture must vary, in levels, for a region to be live.
///
/// Measured over a neighbourhood rather than a single pixel. Per-pixel
/// liveness does not separate the two cases: a *static* graphic is re-encoded
/// every GOP, and the mosquito noise around the edges of its lettering — which
/// is exactly where the locator looks — varies by several levels on its own.
///
/// A neighbourhood is unambiguous, provided it is judged by its *quietest*
/// pixel rather than its average. A logo sits over live video, so everything
/// near it moves, whatever the logo's own opacity. A banner is frozen through
/// and through — and taking the average instead would let its outer edge pass,
/// because that window reaches into the live picture beside it.
const MINIMUM_REGION_LIVENESS: f32 = 8.0;

/// Side of the neighbourhood liveness is averaged over, as a fraction of the
/// frame width.
///
/// Scaled to the picture rather than fixed, because the things being told
/// apart scale with it: a thirtieth of the width is comfortably larger than a
/// logo, so the window always takes in the picture around one, and comfortably
/// smaller than an emergency banner, so a window inside one sees nothing but
/// banner. At 1440 that is 48 pixels.
const LIVENESS_WINDOW_FRACTION: u32 = 30;

/// Accumulates the evidence needed to locate a logo.
#[derive(Debug)]
pub struct LogoLocator {
    width: u32,
    height: u32,
    /// Running sum of edge strength per pixel, for the chunk in progress.
    sum: Vec<f32>,
    /// Running sum of squared edge strength per pixel, for the same chunk.
    sum_squares: Vec<f32>,
    /// Frames in the chunk in progress.
    chunk_frames: u32,
    /// How many chunks each pixel stood out in.
    ///
    /// One counter per pixel rather than one steadiness map per chunk, so the
    /// memory does not grow with the length of the recording.
    votes: Vec<u16>,
    /// Running sum of raw luma per pixel, over the whole recording.
    luma_sum: Vec<f32>,
    /// Running sum of squared raw luma, for the same.
    ///
    /// Kept across the whole recording rather than per chunk: the question is
    /// whether the picture ever moves under this pixel, and a long static shot
    /// inside an otherwise live recording should not condemn it.
    luma_squares: Vec<f32>,
    /// Chunks completed so far.
    chunks: u16,
    frames: u32,
    /// Rows a logo may occupy, computed once.
    rows: VecDeque<u32>,
}

impl LogoLocator {
    /// Start looking for a logo in frames of this size.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let count = (width as usize) * (height as usize);
        let band = ((height as f32) * EDGE_BAND) as u32;
        let band = band.max(2).min(height / 2);
        let rows = (1..band)
            .chain((height - band)..(height.saturating_sub(1)))
            .collect();

        Self {
            width,
            height,
            sum: vec![0.0; count],
            sum_squares: vec![0.0; count],
            chunk_frames: 0,
            votes: vec![0; count],
            luma_sum: vec![0.0; count],
            luma_squares: vec![0.0; count],
            chunks: 0,
            frames: 0,
            rows,
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
        // Only the bands a logo can occupy are scanned; the middle of the
        // picture cannot hold one, and skipping it is most of the work saved.
        for index in 0..self.rows.len() {
            let Some(&y) = self.rows.get(index) else {
                continue;
            };
            for x in 1..self.width - 1 {
                let strength = edge_strength(frame, x, y);
                let offset = (y * self.width + x) as usize;
                if let (Some(sum), Some(squares)) =
                    (self.sum.get_mut(offset), self.sum_squares.get_mut(offset))
                {
                    *sum += strength;
                    *squares += strength * strength;
                }

                // Whether the picture moves under this pixel at all, which is
                // what separates a translucent logo from a frozen graphic.
                let luma = f32::from(frame.pixel(x, y).unwrap_or(0));
                if let (Some(sum), Some(squares)) = (
                    self.luma_sum.get_mut(offset),
                    self.luma_squares.get_mut(offset),
                ) {
                    *sum += luma;
                    *squares += luma * luma;
                }
            }
        }

        self.frames += 1;
        self.chunk_frames += 1;
        if self.chunk_frames >= FRAMES_PER_CHUNK {
            self.close_chunk();
        }
    }

    /// Score the chunk in progress and fold it into the votes.
    fn close_chunk(&mut self) {
        if self.chunk_frames == 0 || self.chunks == u16::MAX {
            return;
        }
        let mut scores = steadiness(&self.sum, &self.sum_squares, self.chunk_frames);

        // Frozen pixels are taken out of the competition before the threshold
        // is set, not merely filtered from the winners. A burned-in graphic's
        // steadiness is unbeatable — its edges never fluctuate at all — so
        // leaving it in raises the bar above anything a real, translucent logo
        // can reach, and the logo gets no votes in any chunk.
        let liveness = self.liveness_map();
        for (score, &alive) in scores.iter_mut().zip(&liveness) {
            if alive < MINIMUM_REGION_LIVENESS {
                *score = 0.0;
            }
        }

        let peak = scores.iter().copied().fold(0.0f32, f32::max);

        if peak > 0.0 {
            let threshold = peak * CHUNK_THRESHOLD;
            for (vote, &score) in self.votes.iter_mut().zip(&scores) {
                if score >= threshold {
                    *vote += 1;
                }
            }
            self.chunks += 1;
        }

        self.sum.fill(0.0);
        self.sum_squares.fill(0.0);
        self.chunk_frames = 0;
    }

    /// The most logo-like region found, if any.
    #[must_use]
    pub fn finish(&mut self) -> Option<Rect> {
        if self.frames < MINIMUM_FRAMES {
            tracing::debug!(frames = self.frames, "too few frames to locate a logo");
            return None;
        }
        // Fold in whatever the last, possibly short, chunk saw.
        self.close_chunk();
        if self.chunks == 0 {
            return None;
        }

        // A pixel is logo-like when it stood out in the majority of chunks.
        // A test pattern held for one stretch of the recording stands out
        // overwhelmingly in its own chunks and not at all in the rest.
        let required = ((f32::from(self.chunks) * REQUIRED_CHUNK_SHARE).ceil() as u16).max(1);

        // A logo is translucent, so the picture moves under it. Anything
        // frozen — an emergency banner, an inset border, a burned-in caption
        // bar — is not a logo however steady it is, and steadiness alone would
        // rank it above the real thing.
        let liveness = self.liveness_map();
        let mut rejected_frozen = 0usize;

        let mut blobs: Vec<(usize, Rect)> = Vec::new();
        let mut visited = vec![false; self.votes.len()];
        for index in 0..self.rows.len() {
            let Some(&y) = self.rows.get(index) else {
                continue;
            };
            for x in 1..self.width - 1 {
                let offset = (y * self.width + x) as usize;
                if visited.get(offset).copied().unwrap_or(true) {
                    continue;
                }
                if self.votes.get(offset).copied().unwrap_or(0) < required {
                    visited[offset] = true;
                    continue;
                }
                if liveness.get(offset).copied().unwrap_or(0.0) < MINIMUM_REGION_LIVENESS {
                    visited[offset] = true;
                    rejected_frozen += 1;
                    continue;
                }
                let (area, rect) = self.flood(&mut visited, &liveness, x, y, required);
                if area >= MINIMUM_BLOB_AREA {
                    blobs.push((area, rect));
                }
            }
        }

        if rejected_frozen > 0 {
            // Worth saying out loud: on a recording made during an emergency
            // broadcast this is most of the picture's furniture, and explains
            // why the logo was found where it was — or not found at all.
            tracing::debug!(
                pixels = rejected_frozen,
                "ignored frozen graphics; the picture does not move under them"
            );
        }

        let cluster = self.best_cluster(&blobs)?;

        // The scanner needs a border of clean background around the logo to
        // read the background colour from, so the region is grown a little.
        Some(cluster.expanded(4, self.width, self.height))
    }

    /// How much the quietest part of each pixel's neighbourhood varies.
    ///
    /// Per-pixel variation first, then eroded over a neighbourhood. The
    /// erosion is what makes this usable: a static graphic's own lettering
    /// carries enough compression noise to look alive pixel by pixel, and its
    /// outer edge would pass any test that averaged in the live picture
    /// alongside. Requiring the whole neighbourhood to be alive admits only
    /// pixels genuinely surrounded by moving picture.
    fn liveness_map(&self) -> Vec<f32> {
        let n = self.frames as f32;
        if n <= 0.0 {
            return vec![0.0; self.luma_sum.len()];
        }

        let per_pixel: Vec<f32> = self
            .luma_sum
            .iter()
            .zip(&self.luma_squares)
            .map(|(&sum, &squares)| {
                let mean = sum / n;
                (squares / n - mean * mean).max(0.0).sqrt()
            })
            .collect();

        self.erode(&per_pixel)
    }

    /// Minimum of `values` over a square neighbourhood, computed separably.
    ///
    /// Two one-dimensional passes rather than one two-dimensional one, so the
    /// cost does not grow with the window's area.
    fn erode(&self, values: &[f32]) -> Vec<f32> {
        let radius = ((self.width / LIVENESS_WINDOW_FRACTION) / 2).max(2);
        let (width, height) = (self.width as usize, self.height as usize);
        let mut horizontal = vec![0.0f32; values.len()];

        for y in 0..height {
            let row = y * width;
            for x in 0..width {
                let from = x.saturating_sub(radius as usize);
                let to = (x + radius as usize + 1).min(width);
                let slice = values.get(row + from..row + to).unwrap_or_default();
                if !slice.is_empty()
                    && let Some(slot) = horizontal.get_mut(row + x)
                {
                    *slot = slice.iter().copied().fold(f32::INFINITY, f32::min);
                }
            }
        }

        let mut out = vec![0.0f32; values.len()];
        for x in 0..width {
            for y in 0..height {
                let from = y.saturating_sub(radius as usize);
                let to = (y + radius as usize + 1).min(height);
                let mut lowest = f32::INFINITY;
                for row in from..to {
                    if let Some(&value) = horizontal.get(row * width + x) {
                        lowest = lowest.min(value);
                    }
                }
                if lowest.is_finite()
                    && let Some(slot) = out.get_mut(y * width + x)
                {
                    *slot = lowest;
                }
            }
        }
        out
    }

    /// Group nearby blobs and return the bounding box of the strongest group.
    ///
    /// Glyphs of one logo sit close together; an unrelated static element
    /// elsewhere in the band does not. Merging by proximity recovers the whole
    /// mark without swallowing the rest of the frame.
    fn best_cluster(&self, blobs: &[(usize, Rect)]) -> Option<Rect> {
        let gap = (self.width / 24).max(8);

        let mut clusters: Vec<(usize, Rect)> = Vec::new();
        for &(area, rect) in blobs {
            // Merge into every cluster this blob is close to, since one blob
            // can bridge two groups that were previously separate.
            let mut merged = (area, rect);
            clusters.retain(|&(other_area, other_rect)| {
                if near(merged.1, other_rect, gap) {
                    merged = (merged.0 + other_area, union(merged.1, other_rect));
                    false
                } else {
                    true
                }
            });
            clusters.push(merged);
        }

        clusters
            .into_iter()
            .filter(|&(area, rect)| area >= MINIMUM_AREA && self.is_logo_shaped(rect))
            .max_by_key(|&(area, _)| area)
            .map(|(_, rect)| rect)
    }

    /// Whether a region's shape and position are consistent with a logo.
    fn is_logo_shaped(&self, rect: Rect) -> bool {
        let max_width = (self.width as f32 * MAXIMUM_WIDTH) as u32;
        let max_height = (self.height as f32 * MAXIMUM_HEIGHT) as u32;
        if rect.width > max_width || rect.height > max_height {
            return false;
        }

        // Logos hug an edge. A candidate floating in from both sides is part
        // of the picture, however steady it is.
        let from_left = rect.x;
        let from_right = self.width.saturating_sub(rect.x + rect.width);
        let margin = self.width / 5;
        from_left <= margin || from_right <= margin
    }

    /// Flood-fill a connected above-threshold region, returning its size and
    /// bounding box.
    ///
    /// The fill is an explicit stack rather than recursion: a region spanning
    /// a large frame would otherwise be deep enough to overflow it.
    fn flood(
        &self,
        visited: &mut [bool],
        liveness: &[f32],
        start_x: u32,
        start_y: u32,
        required: u16,
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
            if self.votes.get(index).copied().unwrap_or(0) < required
                || liveness.get(index).copied().unwrap_or(0.0) < MINIMUM_REGION_LIVENESS
            {
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

/// Fraction of a chunk's peak steadiness a pixel must reach to earn a vote.
const CHUNK_THRESHOLD: f32 = 0.45;

/// Mean edge strength divided by its standard deviation, per pixel.
///
/// High where an edge is both strong and unchanging; low where the picture
/// moves, and low where there is no edge at all.
fn steadiness(sum: &[f32], sum_squares: &[f32], frames: u32) -> Vec<f32> {
    let n = frames as f32;
    if n <= 0.0 {
        return vec![0.0; sum.len()];
    }
    sum.iter()
        .zip(sum_squares)
        .map(|(&total, &squares)| {
            let mean = total / n;
            let variance = (squares / n - mean * mean).max(0.0);
            // The epsilon keeps a perfectly steady edge from dividing by zero,
            // and sets the scale at which "steady" stops mattering.
            mean / (variance.sqrt() + 1.0)
        })
        .collect()
}

/// Whether two rectangles are within `gap` pixels of each other.
fn near(a: Rect, b: Rect, gap: u32) -> bool {
    let horizontal =
        a.x <= b.x + b.width + gap && b.x <= a.x.saturating_add(a.width).saturating_add(gap);
    let vertical =
        a.y <= b.y + b.height + gap && b.y <= a.y.saturating_add(a.height).saturating_add(gap);
    horizontal && vertical
}

/// The smallest rectangle containing both.
fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
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

    /// A frame of moving content, with an optional translucent mark in the
    /// top-left corner standing in for a logo.
    ///
    /// The mark is half-opacity rather than solid, because that is what a
    /// station logo is — and an opaque one would be indistinguishable from
    /// burned-in furniture, which the locator deliberately refuses.
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
                    let index = (y * W + x) as usize;
                    let under = u32::from(luma[index]);
                    luma[index] = u32::midpoint(under, 255) as u8;
                }
            }
        }
        luma
    }

    #[test]
    fn finds_a_translucent_mark_amid_moving_content() {
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
    fn a_logo_made_of_separate_glyphs_is_found_whole() {
        // Japanese station marks are routinely several disconnected pieces —
        // a symbol beside characters that never touch. Taking only the largest
        // connected blob would learn one glyph and miss the rest.
        let glyphs = [
            Rect {
                x: 6,
                y: 6,
                width: 6,
                height: 10,
            },
            Rect {
                x: 15,
                y: 6,
                width: 6,
                height: 10,
            },
            Rect {
                x: 24,
                y: 6,
                width: 6,
                height: 10,
            },
        ];

        let mut locator = LogoLocator::new(W, H);
        for step in 0..200 {
            let mut luma = content(step, None);
            for rect in &glyphs {
                for y in rect.y..rect.y + rect.height {
                    for x in rect.x..rect.x + rect.width {
                        // Translucent, as a station mark is: the picture shows
                        // through, which is what makes it a logo rather than
                        // burned-in furniture.
                        let index = (y * W + x) as usize;
                        let under = u32::from(luma[index]);
                        luma[index] = u32::midpoint(under, 255) as u8;
                    }
                }
            }
            locator.add_frame(&frame(&luma));
        }

        let found = locator.finish().expect("a logo region");
        assert!(found.x <= 6, "should reach the first glyph: {found:?}");
        assert!(
            found.x + found.width >= 30,
            "should reach the last glyph: {found:?}"
        );
        assert!(found.fits_within(W, H));
    }

    #[test]
    fn an_emergency_banner_does_not_beat_the_real_logo() {
        // The L字 layout: an opaque warning banner frozen down the left edge,
        // and a translucent station logo over live picture on the right.
        //
        // By steadiness alone the banner wins — its edges never fluctuate at
        // all, while the logo's move as the picture changes under it. What
        // separates them is that the logo is translucent, so the picture does
        // move underneath.
        let banner = Rect {
            x: 2,
            y: 2,
            width: 14,
            height: 20,
        };
        let logo = Rect {
            x: 96,
            y: 6,
            width: 18,
            height: 12,
        };

        let mut locator = LogoLocator::new(W, H);
        for step in 0..200 {
            let mut luma = content(step, None);

            // The banner: identical in every frame, whatever is behind it.
            for y in banner.y..banner.y + banner.height {
                for x in banner.x..banner.x + banner.width {
                    luma[(y * W + x) as usize] = if (x + y) % 3 == 0 { 235 } else { 30 };
                }
            }

            // The logo: half opacity, so the moving picture shows through.
            for y in logo.y..logo.y + logo.height {
                for x in logo.x..logo.x + logo.width {
                    let index = (y * W + x) as usize;
                    let under = u32::from(luma[index]);
                    luma[index] = u32::midpoint(under, 255) as u8;
                }
            }

            locator.add_frame(&frame(&luma));
        }

        let found = locator.finish().expect("a logo region");
        assert!(
            found.x >= 80,
            "picked the frozen banner instead of the logo: {found:?}"
        );
        assert!(
            found.x <= logo.x && found.x + found.width >= logo.x + logo.width,
            "should cover the logo: {found:?}"
        );
    }

    #[test]
    fn a_frozen_graphic_alone_yields_no_logo() {
        // Nothing but burned-in furniture over moving content. There is no
        // logo here, and inventing one would mis-cut every recording on the
        // channel.
        let mut locator = LogoLocator::new(W, H);
        for step in 0..200 {
            let mut luma = content(step, None);
            for y in 2..24u32 {
                for x in 2..18u32 {
                    luma[(y * W + x) as usize] = if (x + y) % 3 == 0 { 235 } else { 30 };
                }
            }
            locator.add_frame(&frame(&luma));
        }
        assert!(
            locator.finish().is_none(),
            "a frozen graphic is not a logo, however steady"
        );
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
