//! Assembling the candidate positions a commercial block could start or end at.
//!
//! Every boundary comes from one of three sources, and how many of them agree
//! is what tells the segmenter how much to trust a position:
//!
//! - a **logo transition**, which says a boundary is somewhere near here but
//!   is vague about exactly where, because the score is smoothed;
//! - a **scene change**, which is frame-exact but of which a drama has
//!   hundreds;
//! - a **silence**, which broadcast inserts deliberately at junctions.
//!
//! A scene change sitting inside a silence, near a logo transition, is almost
//! certainly a real junction. A scene change on its own is almost certainly
//! not. Recording that support alongside the position is what lets the
//! segmenter weigh them.

use asaborake_analyze::{Analysis, LogoInterval, SceneChange, SilentSpan};
use serde::{Deserialize, Serialize};

/// How close a scene change must be to a silence to count as supported.
pub const DEFAULT_SILENCE_WINDOW: f64 = 0.6;

/// How close two candidates must be before they are treated as one.
pub const DEFAULT_MERGE_WINDOW: f64 = 0.25;

/// Ceiling on scene-change candidates carried into the segmenter.
///
/// A three-hour film has thousands of cuts and the segmenter is quadratic in
/// candidate count. The strongest survive, and every logo- or silence-backed
/// candidate is kept regardless of this cap.
pub const MAX_SCENE_CANDIDATES: usize = 400;

/// A position a segment might begin or end at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Boundary {
    /// Position in the recording, in seconds.
    pub seconds: f64,
    /// A logo interval started or ended near here.
    pub logo_transition: bool,
    /// A scene change happened here.
    pub scene_change: bool,
    /// The audio was silent here.
    pub silence: bool,
    /// Strength of the scene change, 0 when there was none.
    pub strength: f32,
}

impl Boundary {
    /// A boundary at a position with no supporting evidence.
    #[must_use]
    pub const fn bare(seconds: f64) -> Self {
        Self {
            seconds,
            logo_transition: false,
            scene_change: false,
            silence: false,
            strength: 0.0,
        }
    }

    /// How much to trust this position, in `0.0..=1.0`.
    ///
    /// A junction that shows all three signals is as good as it gets; a lone
    /// scene change is worth very little, because there are so many of them.
    #[must_use]
    pub fn quality(&self) -> f64 {
        let mut score = 0.0f64;
        if self.scene_change {
            score += 0.25;
        }
        if self.silence {
            score += 0.4;
        }
        if self.logo_transition {
            score += 0.35;
        }
        score.min(1.0)
    }

    /// Absorb another candidate at effectively the same position.
    fn absorb(&mut self, other: &Self) {
        // A frame-exact scene change is a better position than a smeared logo
        // transition or the middle of a silence, so it wins the tie. This has
        // to be decided before the flags are merged, or `self.scene_change`
        // would already be true and the position would never move.
        let takes_position = other.scene_change && !self.scene_change;

        self.logo_transition |= other.logo_transition;
        self.scene_change |= other.scene_change;
        self.silence |= other.silence;
        self.strength = self.strength.max(other.strength);

        if takes_position {
            self.seconds = other.seconds;
        }
    }
}

/// Build the candidate boundary set for a recording.
#[must_use]
pub fn candidates(analysis: &Analysis, options: &BoundaryOptions) -> Vec<Boundary> {
    let duration = analysis.duration_seconds;
    let mut raw: Vec<Boundary> = Vec::new();

    // The ends of the recording are always boundaries.
    raw.push(Boundary::bare(0.0));
    raw.push(Boundary::bare(duration));

    for interval in &analysis.logo_intervals {
        raw.push(logo_boundary(interval.start));
        raw.push(logo_boundary(interval.end));
    }

    for span in &analysis.silent_spans {
        raw.push(silence_boundary(span));
    }

    for change in scene_candidates(analysis, options) {
        raw.push(Boundary {
            seconds: change.seconds,
            logo_transition: false,
            scene_change: true,
            silence: is_silent_near(
                &analysis.silent_spans,
                change.seconds,
                options.silence_window,
            ),
            strength: change.strength,
        });
    }

    merge(raw, options.merge_window, duration)
}

fn logo_boundary(seconds: f64) -> Boundary {
    Boundary {
        seconds,
        logo_transition: true,
        ..Boundary::bare(seconds)
    }
}

fn silence_boundary(span: &SilentSpan) -> Boundary {
    Boundary {
        seconds: span.centre(),
        silence: true,
        ..Boundary::bare(span.centre())
    }
}

/// Pick the scene changes worth carrying forward.
///
/// Anything backed by silence is kept whatever its strength, because that
/// agreement is the strongest evidence available. The rest compete on strength
/// for the remaining slots.
fn scene_candidates(analysis: &Analysis, options: &BoundaryOptions) -> Vec<SceneChange> {
    let (supported, unsupported): (Vec<_>, Vec<_>) =
        analysis.scene_changes.iter().copied().partition(|change| {
            is_silent_near(
                &analysis.silent_spans,
                change.seconds,
                options.silence_window,
            ) || is_near_logo_transition(
                &analysis.logo_intervals,
                change.seconds,
                options.silence_window,
            )
        });

    let mut ranked = unsupported;
    ranked.sort_unstable_by(|a, b| b.strength.total_cmp(&a.strength));
    ranked.truncate(options.max_scene_candidates.saturating_sub(supported.len()));

    let mut all = supported;
    all.extend(ranked);
    all
}

/// Whether any silence covers, or nearly covers, this moment.
fn is_silent_near(spans: &[SilentSpan], seconds: f64, window: f64) -> bool {
    spans
        .iter()
        .any(|span| seconds >= span.start - window && seconds <= span.end + window)
}

/// Whether a logo interval starts or ends near this moment.
fn is_near_logo_transition(intervals: &[LogoInterval], seconds: f64, window: f64) -> bool {
    intervals.iter().any(|interval| {
        (interval.start - seconds).abs() <= window || (interval.end - seconds).abs() <= window
    })
}

/// Sort, clamp to the recording, and coalesce near-coincident candidates.
fn merge(mut raw: Vec<Boundary>, window: f64, duration: f64) -> Vec<Boundary> {
    raw.retain(|b| b.seconds.is_finite() && b.seconds >= 0.0 && b.seconds <= duration);
    raw.sort_unstable_by(|a, b| a.seconds.total_cmp(&b.seconds));

    let mut merged: Vec<Boundary> = Vec::with_capacity(raw.len());
    for candidate in raw {
        match merged.last_mut() {
            Some(previous) if candidate.seconds - previous.seconds <= window => {
                previous.absorb(&candidate);
            }
            _ => merged.push(candidate),
        }
    }
    merged
}

/// Tunables for candidate construction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundaryOptions {
    /// How close a scene change must be to a silence to count as supported.
    pub silence_window: f64,
    /// How close two candidates must be to become one.
    pub merge_window: f64,
    /// Ceiling on scene-change candidates.
    pub max_scene_candidates: usize,
}

impl Default for BoundaryOptions {
    fn default() -> Self {
        Self {
            silence_window: DEFAULT_SILENCE_WINDOW,
            merge_window: DEFAULT_MERGE_WINDOW,
            max_scene_candidates: MAX_SCENE_CANDIDATES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asaborake_analyze::LogoInterval;

    fn analysis(
        duration: f64,
        logo: &[(f64, f64)],
        scenes: &[(f64, f32)],
        silences: &[(f64, f64)],
    ) -> Analysis {
        Analysis {
            duration_seconds: duration,
            seconds_per_frame: 1.0 / 30.0,
            logo: None,
            learned_logo: None,
            logo_intervals: logo
                .iter()
                .map(|&(start, end)| LogoInterval { start, end })
                .collect(),
            logo_track: None,
            scene_changes: scenes
                .iter()
                .map(|&(seconds, strength)| SceneChange { seconds, strength })
                .collect(),
            silent_spans: silences
                .iter()
                .map(|&(start, end)| SilentSpan { start, end })
                .collect(),
        }
    }

    #[test]
    fn always_includes_both_ends_of_the_recording() {
        let a = analysis(100.0, &[], &[], &[]);
        let found = candidates(&a, &BoundaryOptions::default());
        assert!((found.first().expect("start").seconds - 0.0).abs() < 1e-9);
        assert!((found.last().expect("end").seconds - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_junction_with_all_three_signals_merges_into_one_boundary() {
        // Logo ends at 30.0, a cut at 30.1, silence spanning 29.9..30.3.
        let a = analysis(60.0, &[(0.0, 30.0)], &[(30.1, 50.0)], &[(29.9, 30.3)]);
        let found = candidates(&a, &BoundaryOptions::default());

        let junction = found
            .iter()
            .find(|b| (b.seconds - 30.0).abs() < 0.5)
            .expect("a boundary near 30s");
        assert!(junction.logo_transition, "{junction:?}");
        assert!(junction.scene_change, "{junction:?}");
        assert!(junction.silence, "{junction:?}");
        assert!((junction.quality() - 1.0).abs() < 1e-9);
        // And it lands on the frame-exact scene change, not the logo estimate.
        assert!((junction.seconds - 30.1).abs() < 1e-6, "{junction:?}");
    }

    #[test]
    fn a_lone_scene_change_is_worth_little() {
        let a = analysis(60.0, &[], &[(20.0, 90.0)], &[]);
        let found = candidates(&a, &BoundaryOptions::default());
        let lone = found
            .iter()
            .find(|b| (b.seconds - 20.0).abs() < 0.1)
            .expect("the cut");
        assert!(lone.quality() < 0.3, "{lone:?}");
    }

    #[test]
    fn weak_scene_changes_are_capped_but_supported_ones_always_survive() {
        // A thousand weak cuts, plus one weak cut that coincides with silence.
        let mut scenes: Vec<(f64, f32)> = (0..1000)
            .map(|i| (10.0 + f64::from(i) * 0.5, 20.0))
            .collect();
        scenes.push((700.0, 12.5));
        let a = analysis(1000.0, &[], &scenes, &[(699.8, 700.4)]);

        let options = BoundaryOptions::default();
        let found = candidates(&a, &options);

        assert!(
            found.len() <= options.max_scene_candidates + 8,
            "candidate set not capped: {}",
            found.len()
        );
        assert!(
            found
                .iter()
                .any(|b| (b.seconds - 700.0).abs() < 0.5 && b.silence),
            "a silence-backed cut must survive the cap"
        );
    }

    #[test]
    fn candidates_outside_the_recording_are_dropped() {
        let a = analysis(60.0, &[(-5.0, 80.0)], &[], &[]);
        let found = candidates(&a, &BoundaryOptions::default());
        assert!(found.iter().all(|b| b.seconds >= 0.0 && b.seconds <= 60.0));
    }

    #[test]
    fn boundaries_come_out_sorted_and_distinct() {
        let a = analysis(
            120.0,
            &[(0.0, 30.0), (60.0, 90.0)],
            &[(30.05, 40.0), (59.9, 45.0)],
            &[(29.8, 30.2), (59.7, 60.1)],
        );
        let found = candidates(&a, &BoundaryOptions::default());
        for pair in found.windows(2) {
            assert!(
                pair[1].seconds > pair[0].seconds,
                "not strictly increasing: {pair:?}"
            );
        }
    }
}
