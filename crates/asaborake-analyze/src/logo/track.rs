//! Turning per-frame logo scores into presence intervals.
//!
//! The raw score is noisy: a bright flash, a caption crossing the logo, or a
//! fade will each move it for a few frames. Two things clean it up. A median
//! filter removes short excursions without smearing the real transitions the
//! way an average would — and the exact frame a logo appears on is what a cut
//! point is derived from, so smearing is not acceptable. Then hysteresis, with
//! separate thresholds for turning on and off, stops the state chattering when
//! the score sits near a single threshold.

use serde::{Deserialize, Serialize};

/// Score above which the logo is considered to have appeared.
pub const DEFAULT_ON_THRESHOLD: f32 = 0.45;

/// Score below which it is considered to have gone.
///
/// Lower than the on-threshold on purpose: once the logo is established, it
/// takes more evidence to declare it gone than it took to declare it present.
pub const DEFAULT_OFF_THRESHOLD: f32 = 0.25;

/// Half-width of the median filter, in frames.
///
/// Fifteen frames is half a second at broadcast rates — long enough to absorb
/// a caption sweeping past the logo, short enough not to move a real boundary.
pub const DEFAULT_SMOOTHING_RADIUS: usize = 15;

/// Presence intervals shorter than this are discarded as noise.
pub const DEFAULT_MINIMUM_SECONDS: f64 = 2.0;

/// A span of the recording during which the logo was present.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LogoInterval {
    /// Start, in seconds.
    pub start: f64,
    /// End, in seconds.
    pub end: f64,
}

impl LogoInterval {
    /// Length in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    /// Whether a moment falls inside this interval.
    #[must_use]
    pub fn contains(&self, seconds: f64) -> bool {
        seconds >= self.start && seconds < self.end
    }
}

/// The per-frame logo score across a recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogoTrack {
    /// Seconds between consecutive scores.
    pub seconds_per_frame: f64,
    /// One score per analysed frame.
    pub scores: Vec<f32>,
}

impl LogoTrack {
    /// Time of a frame, in seconds.
    #[must_use]
    pub fn time_of(&self, index: usize) -> f64 {
        index as f64 * self.seconds_per_frame
    }

    /// Total duration covered.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.time_of(self.scores.len())
    }

    /// Median-filtered copy of the scores.
    #[must_use]
    pub fn smoothed(&self, radius: usize) -> Vec<f32> {
        if radius == 0 || self.scores.len() <= 1 {
            return self.scores.clone();
        }
        // A radius wider than the track is meaningless, and a configured one
        // large enough to overflow the window arithmetic would panic.
        let radius = radius.min(self.scores.len());
        let mut window: Vec<f32> = Vec::with_capacity(radius * 2 + 1);
        (0..self.scores.len())
            .map(|index| {
                let from = index.saturating_sub(radius);
                let to = (index + radius + 1).min(self.scores.len());
                window.clear();
                window.extend_from_slice(self.scores.get(from..to).unwrap_or_default());
                window.sort_unstable_by(f32::total_cmp);
                window.get(window.len() / 2).copied().unwrap_or(0.0)
            })
            .collect()
    }

    /// Extract the spans during which the logo was present.
    #[must_use]
    pub fn intervals(&self, options: &TrackOptions) -> Vec<LogoInterval> {
        let smoothed = self.smoothed(options.smoothing_radius);
        let mut intervals: Vec<LogoInterval> = Vec::new();
        let mut start: Option<usize> = None;

        for (index, &score) in smoothed.iter().enumerate() {
            match start {
                None if score >= options.on_threshold => start = Some(index),
                Some(from) if score < options.off_threshold => {
                    intervals.push(self.span(from, index));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            intervals.push(self.span(from, smoothed.len()));
        }

        // Merging comes first. Two four-second spans either side of a
        // one-second occlusion are one programme block, and filtering by
        // duration before joining them would discard both.
        merge_adjacent(intervals, options.merge_gap_seconds)
            .into_iter()
            .filter(|interval| interval.duration() >= options.minimum_seconds)
            .collect()
    }

    /// The interval covering a run of frames.
    fn span(&self, from: usize, to: usize) -> LogoInterval {
        LogoInterval {
            start: self.time_of(from),
            end: self.time_of(to),
        }
    }
}

/// Join intervals separated by less than `gap` seconds.
///
/// A logo briefly obscured by a full-screen caption reads as two intervals;
/// treating that as a programme boundary would cut the programme in half.
fn merge_adjacent(intervals: Vec<LogoInterval>, gap: f64) -> Vec<LogoInterval> {
    let mut merged: Vec<LogoInterval> = Vec::with_capacity(intervals.len());
    for interval in intervals {
        match merged.last_mut() {
            Some(previous) if interval.start - previous.end <= gap => {
                previous.end = interval.end;
            }
            _ => merged.push(interval),
        }
    }
    merged
}

/// Tunables for turning scores into intervals.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackOptions {
    /// Score at which the logo is declared present.
    pub on_threshold: f32,
    /// Score at which it is declared gone.
    pub off_threshold: f32,
    /// Median filter half-width, in frames.
    pub smoothing_radius: usize,
    /// Shortest interval worth keeping, in seconds.
    pub minimum_seconds: f64,
    /// Gap below which two intervals are joined, in seconds.
    pub merge_gap_seconds: f64,
}

impl Default for TrackOptions {
    fn default() -> Self {
        Self {
            on_threshold: DEFAULT_ON_THRESHOLD,
            off_threshold: DEFAULT_OFF_THRESHOLD,
            smoothing_radius: DEFAULT_SMOOTHING_RADIUS,
            minimum_seconds: DEFAULT_MINIMUM_SECONDS,
            // A caption card covering the logo rarely lasts longer than this.
            merge_gap_seconds: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a track at 30 fps from a description of on/off spans in seconds.
    fn track_from(spans: &[(f64, f64, f32)], total: f64) -> LogoTrack {
        let seconds_per_frame = 1.0 / 30.0;
        let frames = (total / seconds_per_frame) as usize;
        let mut scores = vec![-0.6f32; frames];
        for &(start, end, value) in spans {
            let from = (start / seconds_per_frame) as usize;
            let to = ((end / seconds_per_frame) as usize).min(frames);
            for score in scores.get_mut(from..to).unwrap_or_default() {
                *score = value;
            }
        }
        LogoTrack {
            seconds_per_frame,
            scores,
        }
    }

    #[test]
    fn finds_the_span_the_logo_was_present() {
        let track = track_from(&[(10.0, 40.0, 0.8)], 60.0);
        let intervals = track.intervals(&TrackOptions::default());
        assert_eq!(intervals.len(), 1);
        assert!((intervals[0].start - 10.0).abs() < 0.6, "{intervals:?}");
        assert!((intervals[0].end - 40.0).abs() < 0.6, "{intervals:?}");
    }

    #[test]
    fn separates_two_programme_blocks_around_a_commercial() {
        let track = track_from(&[(0.0, 30.0, 0.8), (60.0, 120.0, 0.8)], 120.0);
        let intervals = track.intervals(&TrackOptions::default());
        assert_eq!(intervals.len(), 2, "{intervals:?}");
        assert!((intervals[0].end - 30.0).abs() < 0.6);
        assert!((intervals[1].start - 60.0).abs() < 0.6);
    }

    #[test]
    fn a_brief_dropout_does_not_split_a_programme() {
        // A caption covers the logo for half a second at the 20 second mark.
        let track = track_from(&[(0.0, 20.0, 0.8), (20.5, 60.0, 0.8)], 60.0);
        let intervals = track.intervals(&TrackOptions::default());
        assert_eq!(
            intervals.len(),
            1,
            "a caption must not split it: {intervals:?}"
        );
    }

    #[test]
    fn two_short_spans_around_a_brief_occlusion_survive_as_one() {
        // Neither span reaches the two-second minimum on its own, but the
        // occlusion between them is under a second, so together they are a
        // legitimate interval. Filtering before merging would lose both.
        let track = track_from(&[(10.0, 11.8, 0.8), (12.4, 14.2, 0.8)], 30.0);
        let options = TrackOptions {
            // Smoothing off, so this exercises merging rather than the median
            // filter quietly bridging the gap.
            smoothing_radius: 0,
            ..TrackOptions::default()
        };

        let intervals = track.intervals(&options);
        assert_eq!(intervals.len(), 1, "{intervals:?}");
        assert!((intervals[0].start - 10.0).abs() < 0.1, "{intervals:?}");
        assert!((intervals[0].end - 14.2).abs() < 0.1, "{intervals:?}");
    }

    #[test]
    fn a_short_span_with_no_neighbour_is_still_discarded() {
        let track = track_from(&[(10.0, 11.0, 0.8)], 30.0);
        let options = TrackOptions {
            smoothing_radius: 0,
            ..TrackOptions::default()
        };
        assert!(track.intervals(&options).is_empty());
    }

    #[test]
    fn an_absurd_smoothing_radius_is_clamped_rather_than_overflowing() {
        let track = LogoTrack {
            seconds_per_frame: 1.0 / 30.0,
            scores: vec![0.5; 10],
        };
        let smoothed = track.smoothed(usize::MAX);
        assert_eq!(smoothed.len(), 10);
    }

    #[test]
    fn discards_a_flash_too_short_to_be_a_programme() {
        let track = track_from(&[(10.0, 10.5, 0.9)], 60.0);
        assert!(track.intervals(&TrackOptions::default()).is_empty());
    }

    #[test]
    fn hysteresis_stops_chatter_at_a_single_threshold() {
        // A score oscillating between the two thresholds must hold its state.
        let seconds_per_frame = 1.0 / 30.0;
        let mut scores = vec![0.8f32; 300];
        for (index, score) in scores.iter_mut().enumerate().skip(300 / 2) {
            *score = if index % 2 == 0 { 0.3 } else { 0.4 };
        }
        let track = LogoTrack {
            seconds_per_frame,
            scores,
        };
        let intervals = track.intervals(&TrackOptions::default());
        assert_eq!(
            intervals.len(),
            1,
            "oscillating between thresholds must not chatter: {intervals:?}"
        );
    }

    #[test]
    fn median_smoothing_removes_a_spike_without_moving_an_edge() {
        let mut scores = vec![0.0f32; 100];
        for score in scores.iter_mut().take(100).skip(50) {
            *score = 1.0;
        }
        scores[20] = 1.0; // an isolated spike

        let track = LogoTrack {
            seconds_per_frame: 1.0 / 30.0,
            scores,
        };
        let smoothed = track.smoothed(5);

        assert!((smoothed[20] - 0.0).abs() < 1e-6, "spike survived");
        // The real transition stays where it was, within the filter's radius.
        assert!((smoothed[49] - 0.0).abs() < 1e-6);
        assert!((smoothed[55] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_logo_present_to_the_end_closes_at_the_end() {
        let track = track_from(&[(30.0, 60.0, 0.8)], 60.0);
        let intervals = track.intervals(&TrackOptions::default());
        assert_eq!(intervals.len(), 1);
        assert!(
            (intervals[0].end - track.duration()).abs() < 0.1,
            "{intervals:?}"
        );
    }

    #[test]
    fn interval_membership_is_half_open() {
        let interval = LogoInterval {
            start: 1.0,
            end: 2.0,
        };
        assert!(interval.contains(1.0));
        assert!(interval.contains(1.999));
        assert!(!interval.contains(2.0));
        assert!((interval.duration() - 1.0).abs() < 1e-9);
    }
}
