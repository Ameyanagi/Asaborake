//! Scene-change detection.
//!
//! A commercial boundary is almost always a hard cut, so scene changes supply
//! the candidate positions that logo transitions are snapped onto. On their
//! own they are useless — a drama has hundreds — but combined with silence and
//! a logo transition they locate a boundary to the frame.
//!
//! This corresponds to the scene-change half of Amatsukaze's `chapter_exe`;
//! see `ATTRIBUTION.md`.

use asaborake_media::Frame;
use serde::{Deserialize, Serialize};

/// Width of the reduced frame differences are measured on.
///
/// Comparing full frames measures camera shake and film grain. At this size
/// only a genuine change of content moves the numbers, and the reduction costs
/// almost nothing next to the decode.
pub const COMPARE_WIDTH: u32 = 64;
/// Height of the reduced frame.
pub const COMPARE_HEIGHT: u32 = 36;

/// Minimum mean absolute difference for a cut, on a 0..255 scale.
pub const DEFAULT_ABSOLUTE_THRESHOLD: f32 = 12.0;

/// How many times the local median a difference must reach to be a cut.
///
/// The absolute threshold alone misfires in both directions: a talking-head
/// scene never reaches it even at a real cut, and an action sequence exceeds
/// it constantly. Requiring a multiple of what this part of the recording
/// normally does adapts to both.
pub const DEFAULT_RELATIVE_THRESHOLD: f32 = 3.0;

/// Number of frames either side used for the local median.
const LOCAL_WINDOW: usize = 60;

/// A detected cut.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SceneChange {
    /// When the cut happened, in seconds.
    pub seconds: f64,
    /// How strong it was: the mean absolute difference, 0..255.
    pub strength: f32,
}

/// Accumulates frame-to-frame differences.
#[derive(Debug)]
pub struct SceneDetector {
    previous: Option<Vec<u8>>,
    current: Vec<u8>,
    differences: Vec<f32>,
    seconds_per_frame: f64,
}

impl SceneDetector {
    /// Start detecting, with frames arriving `seconds_per_frame` apart.
    #[must_use]
    pub fn new(seconds_per_frame: f64) -> Self {
        let size = (COMPARE_WIDTH * COMPARE_HEIGHT) as usize;
        Self {
            previous: None,
            current: vec![0; size],
            differences: Vec::new(),
            seconds_per_frame,
        }
    }

    /// Feed the next frame.
    pub fn add_frame(&mut self, frame: &Frame<'_>) {
        frame.downscale_into(COMPARE_WIDTH, COMPARE_HEIGHT, &mut self.current);

        let difference = match &self.previous {
            None => 0.0,
            Some(previous) => mean_absolute_difference(previous, &self.current),
        };
        self.differences.push(difference);

        match &mut self.previous {
            Some(previous) => previous.copy_from_slice(&self.current),
            slot @ None => *slot = Some(self.current.clone()),
        }
    }

    /// The raw per-frame difference series, for display in the timeline.
    #[must_use]
    pub fn differences(&self) -> &[f32] {
        &self.differences
    }

    /// Extract the cuts.
    #[must_use]
    pub fn changes(&self, options: &SceneOptions) -> Vec<SceneChange> {
        let mut changes = Vec::new();

        for (index, &difference) in self.differences.iter().enumerate() {
            if difference < options.absolute_threshold {
                continue;
            }
            let local = local_median(&self.differences, index, LOCAL_WINDOW);
            // The floor keeps a perfectly static stretch — a still title card
            // — from making its local median zero and admitting everything.
            if difference < local.max(1.0) * options.relative_threshold {
                continue;
            }
            // A cut is one frame; the frames either side of a real cut often
            // also exceed the threshold, and only the peak is the boundary.
            if !is_local_peak(&self.differences, index) {
                continue;
            }
            changes.push(SceneChange {
                seconds: index as f64 * self.seconds_per_frame,
                strength: difference,
            });
        }

        changes
    }
}

/// Whether a value is at least as large as its immediate neighbours.
fn is_local_peak(values: &[f32], index: usize) -> bool {
    let value = values.get(index).copied().unwrap_or(0.0);
    let before = index
        .checked_sub(1)
        .and_then(|i| values.get(i))
        .copied()
        .unwrap_or(0.0);
    let after = values.get(index + 1).copied().unwrap_or(0.0);
    value >= before && value >= after
}

/// Median of the values around `index`, excluding `index` itself.
fn local_median(values: &[f32], index: usize, radius: usize) -> f32 {
    let from = index.saturating_sub(radius);
    let to = (index + radius + 1).min(values.len());
    let mut window: Vec<f32> = values
        .get(from..to)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter(|(offset, _)| from + offset != index)
        .map(|(_, &value)| value)
        .collect();
    if window.is_empty() {
        return 0.0;
    }
    window.sort_unstable_by(f32::total_cmp);
    window.get(window.len() / 2).copied().unwrap_or(0.0)
}

/// Mean absolute difference between two equally sized reduced frames.
fn mean_absolute_difference(a: &[u8], b: &[u8]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let total: u32 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| u32::from(x.abs_diff(y)))
        .sum();
    total as f32 / a.len() as f32
}

/// Tunables for cut detection.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SceneOptions {
    /// Minimum mean absolute difference, 0..255.
    pub absolute_threshold: f32,
    /// Multiple of the local median a difference must reach.
    pub relative_threshold: f32,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            absolute_threshold: DEFAULT_ABSOLUTE_THRESHOLD,
            relative_threshold: DEFAULT_RELATIVE_THRESHOLD,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 128;
    const H: u32 = 72;

    fn frame(luma: &[u8]) -> Frame<'_> {
        Frame {
            index: 0,
            timestamp: 0.0,
            width: W,
            height: H,
            luma,
        }
    }

    /// A frame of the given base brightness with a little moving texture.
    fn shot(brightness: u8, jitter: u32) -> Vec<u8> {
        (0..(W * H))
            .map(|i| {
                let wobble = ((i + jitter) % 5) as u8;
                brightness.saturating_add(wobble)
            })
            .collect()
    }

    #[test]
    fn finds_a_hard_cut_between_two_shots() {
        let mut detector = SceneDetector::new(1.0 / 30.0);
        for jitter in 0..60 {
            let luma = shot(40, jitter);
            detector.add_frame(&frame(&luma));
        }
        for jitter in 0..60 {
            let luma = shot(200, jitter);
            detector.add_frame(&frame(&luma));
        }

        let changes = detector.changes(&SceneOptions::default());
        assert_eq!(changes.len(), 1, "{changes:?}");
        // The cut is at frame 60, i.e. two seconds in at 30 fps.
        assert!((changes[0].seconds - 2.0).abs() < 0.05, "{changes:?}");
        assert!(changes[0].strength > 100.0, "{changes:?}");
    }

    #[test]
    fn reports_nothing_on_a_continuous_shot() {
        let mut detector = SceneDetector::new(1.0 / 30.0);
        for jitter in 0..150 {
            let luma = shot(120, jitter);
            detector.add_frame(&frame(&luma));
        }
        assert!(
            detector.changes(&SceneOptions::default()).is_empty(),
            "gentle motion is not a cut"
        );
    }

    #[test]
    fn a_cut_reports_one_frame_not_a_cluster() {
        let mut detector = SceneDetector::new(1.0 / 30.0);
        for jitter in 0..40 {
            detector.add_frame(&frame(&shot(30, jitter)));
        }
        for jitter in 0..40 {
            detector.add_frame(&frame(&shot(220, jitter)));
        }
        for jitter in 0..40 {
            detector.add_frame(&frame(&shot(30, jitter)));
        }
        let changes = detector.changes(&SceneOptions::default());
        assert_eq!(changes.len(), 2, "expected two cuts, got {changes:?}");
    }

    #[test]
    fn mean_absolute_difference_is_zero_for_identical_frames() {
        let a = vec![10u8, 20, 30];
        assert!(mean_absolute_difference(&a, &a).abs() < f32::EPSILON);
        assert!((mean_absolute_difference(&a, &[20, 30, 40]) - 10.0).abs() < f32::EPSILON);
        // Mismatched sizes are a programming error, not a huge difference.
        assert!(mean_absolute_difference(&a, &[1, 2]).abs() < f32::EPSILON);
    }

    #[test]
    fn local_median_excludes_the_point_being_tested() {
        // Without the exclusion, an isolated spike raises its own baseline.
        let values = vec![1.0f32, 1.0, 1.0, 100.0, 1.0, 1.0, 1.0];
        assert!((local_median(&values, 3, 3) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn only_the_peak_frame_of_a_transition_counts() {
        let values = vec![0.0f32, 5.0, 20.0, 5.0, 0.0];
        assert!(is_local_peak(&values, 2));
        assert!(!is_local_peak(&values, 1));
        assert!(!is_local_peak(&values, 3));
    }
}
