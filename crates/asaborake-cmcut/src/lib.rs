//! Deciding which parts of a recording are commercials.
//!
//! # Why an optimisation rather than rules
//!
//! `join_logo_scp`, which this replaces, works as a cascade of rules: look for
//! this pattern, then that one, then patch up the leftovers. That is how the
//! problem is usually described, and it works — but each rule interacts with
//! every other, so tuning one changes the behaviour of the rest, and there is
//! no way to ask what the *best* interpretation of a recording is.
//!
//! Asaborake states the problem once instead. A recording is a sequence of
//! segments, each either programme or commercial, split at candidate
//! boundaries. Every candidate segmentation has a score, built from the
//! evidence:
//!
//! - the logo should be present through programme and absent through
//!   commercials;
//! - Japanese commercial blocks are laid out on a **15-second grid**, so a
//!   block of 30, 60 or 90 seconds is far more plausible than one of 47;
//! - a boundary where a scene change, a silence and a logo transition all
//!   coincide is worth much more than a lone scene change;
//! - programmes are long, commercial blocks are not, and neither alternates
//!   every few seconds.
//!
//! The best-scoring segmentation is then found exactly, by dynamic
//! programming, in time quadratic in the number of candidate boundaries. Every
//! rule above is one term with one weight, and changing a weight changes only
//! that term.
//!
//! # Refusing to guess
//!
//! The segmenter also reports how much it trusts its own answer, and the
//! default policy on a low score is to **keep the whole recording** and write
//! chapters instead of cutting. A recording that keeps its commercials is a
//! minor annoyance; one whose programme was cut away is gone.
//!
//! This corresponds to Amatsukaze's `join_logo_scp`; see `ATTRIBUTION.md`.

// Tests assert; asserting is how they fail. The workspace bans panicking
// constructs in shipping code, not in the suite that checks it.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod boundary;

use asaborake_analyze::Analysis;
use serde::{Deserialize, Serialize};

pub use boundary::{Boundary, BoundaryOptions};

/// What a stretch of the recording is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    /// Part of the programme; kept.
    Programme,
    /// A commercial block; cut, when the plan is applied.
    Commercial,
}

impl SegmentKind {
    /// The other kind.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Programme => Self::Commercial,
            Self::Commercial => Self::Programme,
        }
    }

    /// A short label for chapter titles.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Programme => "Programme",
            Self::Commercial => "CM",
        }
    }
}

/// One labelled stretch of the recording.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Start, in seconds from the beginning of the recording.
    pub start: f64,
    /// End, in seconds.
    pub end: f64,
    /// What this stretch is.
    pub kind: SegmentKind,
    /// How much the segmenter trusts this label, in `0.0..=1.0`.
    pub confidence: f64,
}

impl Segment {
    /// Length in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// What to do when the segmenter does not trust its own answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LowConfidencePolicy {
    /// Transcode the whole recording, writing chapters but cutting nothing.
    ///
    /// The default, because an uncut recording is an annoyance and a
    /// wrongly cut one is a loss.
    Keep,
    /// Cut anyway.
    Cut,
    /// Fail the job so a human looks at it.
    Fail,
}

/// Whether the plan's cuts should be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Cut the commercials out.
    Cut,
    /// Keep everything, and rely on the chapters to mark the commercials.
    KeepAll,
}

/// A stretch of the recording to keep.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KeepRange {
    /// Start, in seconds.
    pub start: f64,
    /// End, in seconds.
    pub end: f64,
}

impl KeepRange {
    /// Length in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// The segmenter's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CutPlan {
    /// Every segment, in order, covering the whole recording.
    pub segments: Vec<Segment>,
    /// The stretches to keep, merged and in order.
    pub keep: Vec<KeepRange>,
    /// Overall confidence, in `0.0..=1.0`.
    pub confidence: f64,
    /// Whether the cuts should be applied.
    pub decision: Decision,
    /// Why, in a form suitable for a log line or the web UI.
    pub reason: String,
}

impl CutPlan {
    /// Total duration of the kept material, in seconds.
    #[must_use]
    pub fn kept_seconds(&self) -> f64 {
        self.keep.iter().map(KeepRange::duration).sum()
    }

    /// Total duration that would be cut, in seconds.
    #[must_use]
    pub fn cut_seconds(&self) -> f64 {
        self.segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Commercial)
            .map(Segment::duration)
            .sum()
    }

    /// Whether anything would actually be removed.
    #[must_use]
    pub fn cuts_anything(&self) -> bool {
        self.decision == Decision::Cut && self.cut_seconds() > 0.0
    }
}

/// Weights for the scoring terms, in units of "seconds of evidence".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    /// Per second, for the logo agreeing with the label.
    pub logo: f64,
    /// For a commercial block landing on the 15-second grid.
    pub grid_bonus: f64,
    /// For a commercial block that does not.
    pub off_grid_penalty: f64,
    /// Charged at every boundary, to discourage over-segmentation.
    pub switch_penalty: f64,
    /// Multiplied by boundary quality and credited at every boundary.
    pub boundary_bonus: f64,
    /// For a programme segment shorter than [`CutOptions::minimum_programme`].
    pub short_programme_penalty: f64,
    /// Per second, for a commercial block longer than
    /// [`CutOptions::maximum_commercial`].
    pub overlong_commercial_penalty: f64,
    /// For a commercial block shorter than
    /// [`CutOptions::minimum_commercial`].
    pub tiny_commercial_penalty: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            logo: 1.0,
            grid_bonus: 20.0,
            off_grid_penalty: 15.0,
            switch_penalty: 8.0,
            boundary_bonus: 30.0,
            short_programme_penalty: 40.0,
            overlong_commercial_penalty: 2.0,
            tiny_commercial_penalty: 30.0,
        }
    }
}

/// Tunables for segmentation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CutOptions {
    /// The unit Japanese commercial blocks are laid out in, in seconds.
    pub grid_seconds: f64,
    /// How far off the grid a block may fall and still count as on it.
    pub grid_tolerance: f64,
    /// Longest plausible commercial block, in seconds.
    pub maximum_commercial: f64,
    /// Shortest plausible commercial block, in seconds.
    pub minimum_commercial: f64,
    /// Shortest plausible programme segment, in seconds.
    pub minimum_programme: f64,
    /// Confidence below which [`LowConfidencePolicy`] takes over.
    pub confidence_threshold: f64,
    /// What to do below that threshold.
    pub low_confidence: LowConfidencePolicy,
    /// Scoring weights.
    pub weights: Weights,
    /// Candidate construction tunables.
    pub boundaries: BoundaryOptions,
}

impl Default for CutOptions {
    fn default() -> Self {
        Self {
            grid_seconds: 15.0,
            // Broadcast timing is tight but the boundary itself is only known
            // to within a scene change, so a third of a second is realistic.
            grid_tolerance: 0.35,
            maximum_commercial: 180.0,
            minimum_commercial: 10.0,
            minimum_programme: 60.0,
            confidence_threshold: 0.55,
            low_confidence: LowConfidencePolicy::Keep,
            weights: Weights::default(),
            boundaries: BoundaryOptions::default(),
        }
    }
}

/// Segment a recording into programme and commercial stretches.
#[must_use]
pub fn plan(analysis: &Analysis, options: &CutOptions) -> CutPlan {
    let boundaries = boundary::candidates(analysis, &options.boundaries);
    let logo_available = analysis.has_logo();

    if boundaries.len() < 2 || analysis.duration_seconds <= 0.0 {
        return keep_everything(analysis, "the recording has no usable structure");
    }

    let coverage = LogoCoverage::new(analysis, &boundaries);
    let Some(segments) = solve(&boundaries, &coverage, logo_available, options) else {
        return keep_everything(analysis, "no segmentation could be scored");
    };

    let confidence = confidence_of(&segments, logo_available);
    let (decision, reason) = decide(confidence, &segments, logo_available, options);

    let keep = match decision {
        Decision::Cut => merge_kept(&segments),
        Decision::KeepAll => vec![KeepRange {
            start: 0.0,
            end: analysis.duration_seconds,
        }],
    };

    CutPlan {
        segments,
        keep,
        confidence,
        decision,
        reason,
    }
}

/// The fallback plan: change nothing.
fn keep_everything(analysis: &Analysis, reason: &str) -> CutPlan {
    let whole = KeepRange {
        start: 0.0,
        end: analysis.duration_seconds,
    };
    CutPlan {
        segments: vec![Segment {
            start: 0.0,
            end: analysis.duration_seconds,
            kind: SegmentKind::Programme,
            confidence: 0.0,
        }],
        keep: vec![whole],
        confidence: 0.0,
        decision: Decision::KeepAll,
        reason: reason.to_owned(),
    }
}

/// Prefix sums of logo presence, so segment coverage is an O(1) lookup.
struct LogoCoverage {
    /// Seconds of logo present from the start of the recording to each
    /// boundary.
    prefix: Vec<f64>,
}

impl LogoCoverage {
    fn new(analysis: &Analysis, boundaries: &[Boundary]) -> Self {
        let prefix = boundaries
            .iter()
            .map(|boundary| {
                analysis
                    .logo_intervals
                    .iter()
                    .map(|interval| (interval.end.min(boundary.seconds) - interval.start).max(0.0))
                    .sum()
            })
            .collect();
        Self { prefix }
    }

    /// Fraction of `[from, to)` the logo was present for.
    fn fraction(&self, from: usize, to: usize, seconds: f64) -> f64 {
        if seconds <= 0.0 {
            return 0.0;
        }
        let (Some(&start), Some(&end)) = (self.prefix.get(from), self.prefix.get(to)) else {
            return 0.0;
        };
        ((end - start) / seconds).clamp(0.0, 1.0)
    }
}

/// One cell of the dynamic-programming table.
#[derive(Debug, Clone, Copy)]
struct Cell {
    score: f64,
    /// Boundary index this segment started at.
    from: usize,
    reachable: bool,
}

/// Find the highest-scoring segmentation.
///
/// The table is indexed by (boundary, label of the segment ending there).
/// Adjacent segments always differ in label — two adjacent segments with the
/// same label are just one longer segment, so allowing them would add no
/// expressiveness and double the work.
fn solve(
    boundaries: &[Boundary],
    coverage: &LogoCoverage,
    logo_available: bool,
    options: &CutOptions,
) -> Option<Vec<Segment>> {
    const KINDS: [SegmentKind; 2] = [SegmentKind::Programme, SegmentKind::Commercial];
    let n = boundaries.len();

    let mut table = vec![
        [Cell {
            score: f64::NEG_INFINITY,
            from: 0,
            reachable: false,
        }; 2];
        n
    ];

    for j in 1..n {
        for (k, &kind) in KINDS.iter().enumerate() {
            let mut best = Cell {
                score: f64::NEG_INFINITY,
                from: 0,
                reachable: false,
            };

            for i in 0..j {
                let (Some(from), Some(to)) = (boundaries.get(i), boundaries.get(j)) else {
                    continue;
                };
                let seconds = to.seconds - from.seconds;
                if seconds <= 0.0 {
                    continue;
                }

                // Reaching `i` costs whatever the best segmentation ending
                // there with the opposite label cost, plus the price of the
                // boundary itself. `i == 0` is the start of the recording,
                // which is free.
                let previous = if i == 0 {
                    0.0
                } else {
                    let opposite = table.get(i)?.get(1 - k)?;
                    if !opposite.reachable {
                        continue;
                    }
                    opposite.score
                        + from.quality().mul_add(
                            options.weights.boundary_bonus,
                            -options.weights.switch_penalty,
                        )
                };

                let fraction = coverage.fraction(i, j, seconds);
                let score =
                    previous + segment_score(seconds, fraction, kind, logo_available, options);

                if score > best.score {
                    best = Cell {
                        score,
                        from: i,
                        reachable: true,
                    };
                }
            }

            *table.get_mut(j)?.get_mut(k)? = best;
        }
    }

    // Walk back from whichever label ends the recording better.
    let last = table.get(n - 1)?;
    let mut kind_index = usize::from(last.first()?.score < last.get(1)?.score);
    if !last.get(kind_index)?.reachable {
        return None;
    }

    let mut segments = Vec::new();
    let mut j = n - 1;
    while j > 0 {
        let cell = *table.get(j)?.get(kind_index)?;
        if !cell.reachable {
            return None;
        }
        let i = cell.from;
        let (from, to) = (boundaries.get(i)?, boundaries.get(j)?);
        let seconds = to.seconds - from.seconds;
        let fraction = coverage.fraction(i, j, seconds);
        let kind = *KINDS.get(kind_index)?;

        segments.push(Segment {
            start: from.seconds,
            end: to.seconds,
            kind,
            confidence: segment_confidence(
                seconds,
                fraction,
                kind,
                from,
                to,
                logo_available,
                options,
            ),
        });

        j = i;
        kind_index = 1 - kind_index;
    }

    segments.reverse();
    Some(segments)
}

/// Score one candidate segment.
fn segment_score(
    seconds: f64,
    logo_fraction: f64,
    kind: SegmentKind,
    logo_available: bool,
    options: &CutOptions,
) -> f64 {
    let weights = &options.weights;

    // With no logo to go on, the logo term must contribute nothing rather than
    // penalising every segment equally — otherwise "logo absent everywhere"
    // reads as "commercials everywhere".
    let logo_term = if logo_available {
        let agreement = match kind {
            SegmentKind::Programme => logo_fraction.mul_add(2.0, -1.0),
            SegmentKind::Commercial => logo_fraction.mul_add(-2.0, 1.0),
        };
        seconds * weights.logo * agreement
    } else {
        0.0
    };

    match kind {
        SegmentKind::Programme => {
            let short = if seconds < options.minimum_programme {
                weights.short_programme_penalty
            } else {
                0.0
            };
            logo_term - short
        }
        SegmentKind::Commercial => {
            let grid = if fits_grid(seconds, options) {
                weights.grid_bonus
            } else {
                -weights.off_grid_penalty
            };
            let overlong = (seconds - options.maximum_commercial).max(0.0)
                * weights.overlong_commercial_penalty;
            let tiny = if seconds < options.minimum_commercial {
                weights.tiny_commercial_penalty
            } else {
                0.0
            };
            logo_term + grid - overlong - tiny
        }
    }
}

/// Whether a duration is a whole number of grid units, within tolerance.
fn fits_grid(seconds: f64, options: &CutOptions) -> bool {
    if options.grid_seconds <= 0.0 {
        return false;
    }
    let units = (seconds / options.grid_seconds).round();
    if units < 1.0 {
        return false;
    }
    (seconds - units * options.grid_seconds).abs() <= options.grid_tolerance
}

/// How much to trust one segment's label.
fn segment_confidence(
    seconds: f64,
    logo_fraction: f64,
    kind: SegmentKind,
    from: &Boundary,
    to: &Boundary,
    logo_available: bool,
    options: &CutOptions,
) -> f64 {
    let agreement = if logo_available {
        match kind {
            SegmentKind::Programme => logo_fraction,
            SegmentKind::Commercial => 1.0 - logo_fraction,
        }
    } else {
        // Without a logo the label rests entirely on timing and boundaries,
        // which is genuinely weaker evidence.
        0.35
    };

    let grid = match kind {
        SegmentKind::Commercial if fits_grid(seconds, options) => 1.0,
        SegmentKind::Commercial => 0.0,
        // A programme has no expected duration, so the grid says nothing
        // either way and should not drag its confidence down.
        SegmentKind::Programme => 0.6,
    };

    let edges = f64::midpoint(from.quality(), to.quality());

    0.3f64.mul_add(grid, 0.45f64.mul_add(agreement, 0.25 * edges))
}

/// Overall confidence: the duration-weighted mean over the commercial blocks,
/// since those are the segments whose mislabelling loses material.
fn confidence_of(segments: &[Segment], logo_available: bool) -> f64 {
    let commercials: Vec<&Segment> = segments
        .iter()
        .filter(|s| s.kind == SegmentKind::Commercial)
        .collect();

    if commercials.is_empty() {
        // Finding nothing to cut is a real answer, and a confident one when
        // the logo was visible throughout.
        return if logo_available { 0.9 } else { 0.5 };
    }

    let total: f64 = commercials.iter().map(|s| s.duration()).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let weighted = commercials
        .iter()
        .map(|s| s.confidence * s.duration())
        .sum::<f64>()
        / total;

    if logo_available {
        return weighted;
    }

    // Without a logo the evidence is timing and boundaries alone. That is
    // enough to *propose* a segmentation — and on a real recording it produces
    // blocks that land on the fifteen-second grid convincingly — but it is not
    // enough to remove material irreversibly, because nothing in it
    // distinguishes a commercial break from a scene change that happens to sit
    // on the grid between two silences.
    //
    // So it is capped below any sensible threshold. The plan is still
    // computed, still shown, and still written as chapters; an operator who
    // wants it applied anyway sets the policy to cut.
    weighted.min(LOGO_FREE_CONFIDENCE_CEILING)
}

/// The most a plan with no logo evidence behind it may claim.
///
/// Below any sensible value of [`CutOptions::confidence_threshold`], so the
/// low-confidence policy always governs the logo-free case.
pub const LOGO_FREE_CONFIDENCE_CEILING: f64 = 0.5;

/// Apply the low-confidence policy.
fn decide(
    confidence: f64,
    segments: &[Segment],
    logo_available: bool,
    options: &CutOptions,
) -> (Decision, String) {
    let commercials = segments
        .iter()
        .filter(|s| s.kind == SegmentKind::Commercial)
        .count();

    if commercials == 0 {
        return (
            Decision::Cut,
            "no commercial blocks found; nothing to cut".to_owned(),
        );
    }
    if confidence >= options.confidence_threshold {
        return (
            Decision::Cut,
            format!("{commercials} commercial blocks, confidence {confidence:.2}"),
        );
    }

    let why = if logo_available {
        format!(
            "confidence {confidence:.2} is below {:.2}",
            options.confidence_threshold
        )
    } else {
        format!(
            "no logo was found, and confidence {confidence:.2} is below {:.2}",
            options.confidence_threshold
        )
    };

    match options.low_confidence {
        LowConfidencePolicy::Keep => (
            Decision::KeepAll,
            format!("{why}; keeping the whole recording and marking chapters"),
        ),
        LowConfidencePolicy::Cut => (
            Decision::Cut,
            format!("{why}; cutting anyway as configured"),
        ),
        LowConfidencePolicy::Fail => (Decision::KeepAll, format!("{why}; configured to fail")),
    }
}

/// Merge the programme segments into contiguous keep ranges.
fn merge_kept(segments: &[Segment]) -> Vec<KeepRange> {
    let mut keep: Vec<KeepRange> = Vec::new();
    for segment in segments.iter().filter(|s| s.kind == SegmentKind::Programme) {
        match keep.last_mut() {
            Some(previous) if (segment.start - previous.end).abs() < 1e-6 => {
                previous.end = segment.end;
            }
            _ => keep.push(KeepRange {
                start: segment.start,
                end: segment.end,
            }),
        }
    }
    keep
}

/// Errors from segmentation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The plan's confidence was below the threshold and the configured
    /// policy is to fail rather than guess.
    #[error("commercial detection was not confident enough: {reason}")]
    LowConfidence {
        /// What the plan reported.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use asaborake_analyze::{LogoInterval, SceneChange, SilentSpan};

    /// Build an analysis describing a programme with commercial breaks.
    ///
    /// Each break gets a silence and a scene change at both ends, as broadcast
    /// actually produces, and the logo is absent across it.
    fn broadcast(duration: f64, breaks: &[(f64, f64)], with_logo: bool) -> Analysis {
        let mut logo_intervals = Vec::new();
        let mut scene_changes = Vec::new();
        let mut silent_spans = Vec::new();

        let mut cursor = 0.0;
        for &(start, end) in breaks {
            if with_logo && start > cursor {
                logo_intervals.push(LogoInterval {
                    start: cursor,
                    end: start,
                });
            }
            for &edge in &[start, end] {
                scene_changes.push(SceneChange {
                    seconds: edge,
                    strength: 90.0,
                });
                silent_spans.push(SilentSpan {
                    start: edge - 0.2,
                    end: edge + 0.2,
                });
            }
            cursor = end;
        }
        if with_logo && cursor < duration {
            logo_intervals.push(LogoInterval {
                start: cursor,
                end: duration,
            });
        }

        // Ordinary cuts inside the programme, which must not be mistaken for
        // block boundaries.
        let mut seconds = 7.0;
        while seconds < duration {
            scene_changes.push(SceneChange {
                seconds,
                strength: 25.0,
            });
            seconds += 13.0;
        }
        scene_changes.sort_unstable_by(|a, b| a.seconds.total_cmp(&b.seconds));

        Analysis {
            duration_seconds: duration,
            seconds_per_frame: 1.0 / 30.0,
            learned_logo: None,
            logo: with_logo.then_some(asaborake_analyze::LogoSummary {
                rect: asaborake_analyze::Rect {
                    x: 0,
                    y: 0,
                    width: 32,
                    height: 32,
                },
                mean_alpha: 0.5,
                frames_used: 500,
                from_store: false,
            }),
            logo_intervals,
            logo_track: None,
            scene_changes,
            silent_spans,
        }
    }

    #[test]
    fn finds_a_single_commercial_break() {
        // 10 minutes, with a 60-second break at 5 minutes.
        let analysis = broadcast(600.0, &[(300.0, 360.0)], true);
        let plan = plan(&analysis, &CutOptions::default());

        assert_eq!(plan.decision, Decision::Cut, "{}", plan.reason);
        let commercials: Vec<_> = plan
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Commercial)
            .collect();
        assert_eq!(commercials.len(), 1, "{:?}", plan.segments);
        assert!(
            (commercials[0].start - 300.0).abs() < 1.0,
            "{commercials:?}"
        );
        assert!((commercials[0].end - 360.0).abs() < 1.0, "{commercials:?}");
        assert!(plan.confidence > 0.6, "confidence {}", plan.confidence);
    }

    #[test]
    fn finds_several_breaks_and_keeps_the_programme_around_them() {
        let analysis = broadcast(
            1800.0,
            &[(420.0, 510.0), (960.0, 1050.0), (1500.0, 1560.0)],
            true,
        );
        let plan = plan(&analysis, &CutOptions::default());

        assert_eq!(plan.decision, Decision::Cut, "{}", plan.reason);
        let commercials: Vec<_> = plan
            .segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Commercial)
            .collect();
        assert_eq!(commercials.len(), 3, "{:?}", plan.segments);

        // 90 + 90 + 60 seconds removed.
        assert!(
            (plan.cut_seconds() - 240.0).abs() < 3.0,
            "{}",
            plan.cut_seconds()
        );
        assert!(
            (plan.kept_seconds() - 1560.0).abs() < 3.0,
            "{}",
            plan.kept_seconds()
        );
    }

    #[test]
    fn a_programme_with_no_breaks_is_left_alone() {
        let analysis = broadcast(1800.0, &[], true);
        let plan = plan(&analysis, &CutOptions::default());

        assert!(plan.cut_seconds().abs() < 1e-9, "{:?}", plan.segments);
        assert!(plan.confidence > 0.8, "confidence {}", plan.confidence);
        assert!(!plan.cuts_anything());
    }

    #[test]
    fn segments_tile_the_whole_recording_without_gaps() {
        let analysis = broadcast(1200.0, &[(300.0, 390.0), (800.0, 860.0)], true);
        let plan = plan(&analysis, &CutOptions::default());

        assert!((plan.segments.first().expect("a segment").start - 0.0).abs() < 1e-6);
        assert!((plan.segments.last().expect("a segment").end - 1200.0).abs() < 1e-6);
        for pair in plan.segments.windows(2) {
            assert!(
                (pair[1].start - pair[0].end).abs() < 1e-6,
                "gap between {:?} and {:?}",
                pair[0],
                pair[1]
            );
            assert_ne!(
                pair[0].kind, pair[1].kind,
                "adjacent segments must alternate"
            );
        }
    }

    #[test]
    fn without_a_logo_it_keeps_the_recording_rather_than_guessing() {
        let analysis = broadcast(1800.0, &[(420.0, 510.0), (960.0, 1050.0)], false);
        let plan = plan(&analysis, &CutOptions::default());

        assert_eq!(
            plan.decision,
            Decision::KeepAll,
            "reason: {} confidence: {}",
            plan.reason,
            plan.confidence
        );
        assert_eq!(plan.keep.len(), 1);
        assert!((plan.kept_seconds() - 1800.0).abs() < 1e-6);
    }

    #[test]
    fn a_convincing_logo_free_plan_still_will_not_cut_on_its_own() {
        // Observed on a real twenty-minute recording made during an emergency
        // broadcast, where no logo could be found: the segmenter produced
        // twelve blocks, every one an exact multiple of fifteen seconds, and
        // the duration-weighted confidence came out just above the threshold.
        //
        // It is a plausible reading. It is not evidence enough to delete a
        // fifth of somebody's recording, because nothing in timing alone
        // separates a commercial break from a scene change that happens to
        // fall on the grid between two silences.
        let breaks: Vec<(f64, f64)> = (0..12)
            .map(|i| {
                let start = 90.0 + f64::from(i) * 150.0;
                (start, start + 60.0)
            })
            .collect();
        let analysis = broadcast(1900.0, &breaks, false);

        let plan = plan(&analysis, &CutOptions::default());
        assert!(
            plan.confidence <= LOGO_FREE_CONFIDENCE_CEILING,
            "logo-free confidence must stay capped, got {}",
            plan.confidence
        );
        assert_eq!(
            plan.decision,
            Decision::KeepAll,
            "reason: {} confidence: {}",
            plan.reason,
            plan.confidence
        );
        // The plan itself survives, so it can be shown and written as chapters.
        assert!(
            plan.segments
                .iter()
                .any(|s| s.kind == SegmentKind::Commercial),
            "the reading is still reported, just not applied"
        );
    }

    #[test]
    fn the_cut_anyway_policy_overrides_low_confidence() {
        let analysis = broadcast(1800.0, &[(420.0, 510.0), (960.0, 1050.0)], false);
        let options = CutOptions {
            low_confidence: LowConfidencePolicy::Cut,
            ..CutOptions::default()
        };
        let plan = plan(&analysis, &options);
        assert_eq!(plan.decision, Decision::Cut, "{}", plan.reason);
    }

    #[test]
    fn the_fifteen_second_grid_is_what_distinguishes_a_block() {
        let options = CutOptions::default();
        for seconds in [15.0, 30.0, 60.0, 90.0, 120.0] {
            assert!(fits_grid(seconds, &options), "{seconds}s should fit");
        }
        // Within tolerance of the grid.
        assert!(fits_grid(60.3, &options));
        // And clearly off it.
        for seconds in [47.0, 61.0, 22.5] {
            assert!(!fits_grid(seconds, &options), "{seconds}s should not fit");
        }
        // A block shorter than one unit is not on the grid at all.
        assert!(!fits_grid(3.0, &options));
    }

    #[test]
    fn an_empty_analysis_keeps_everything() {
        let analysis = Analysis {
            duration_seconds: 0.0,
            seconds_per_frame: 0.0,
            logo: None,
            learned_logo: None,
            logo_intervals: Vec::new(),
            logo_track: None,
            scene_changes: Vec::new(),
            silent_spans: Vec::new(),
        };
        let plan = plan(&analysis, &CutOptions::default());
        assert_eq!(plan.decision, Decision::KeepAll);
        assert!(plan.confidence.abs() < 1e-9);
    }

    #[test]
    fn keep_ranges_merge_adjacent_programme_segments() {
        let segments = vec![
            Segment {
                start: 0.0,
                end: 10.0,
                kind: SegmentKind::Programme,
                confidence: 1.0,
            },
            Segment {
                start: 10.0,
                end: 20.0,
                kind: SegmentKind::Commercial,
                confidence: 1.0,
            },
            Segment {
                start: 20.0,
                end: 30.0,
                kind: SegmentKind::Programme,
                confidence: 1.0,
            },
        ];
        let keep = merge_kept(&segments);
        assert_eq!(keep.len(), 2);
        assert!((keep[0].duration() - 10.0).abs() < 1e-9);
        assert!((keep[1].start - 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_break_beyond_the_plausible_maximum_is_penalised_for_its_excess() {
        let strict = CutOptions::default();
        let permissive = CutOptions {
            maximum_commercial: 900.0,
            ..strict
        };

        // The same ten-minute block, scored against a maximum it exceeds and
        // one it does not. The difference is exactly the excess penalty.
        let penalised = segment_score(600.0, 0.0, SegmentKind::Commercial, true, &strict);
        let unpenalised = segment_score(600.0, 0.0, SegmentKind::Commercial, true, &permissive);
        let excess =
            (600.0 - strict.maximum_commercial) * strict.weights.overlong_commercial_penalty;

        assert!(penalised < unpenalised, "{penalised} vs {unpenalised}");
        assert!(
            (unpenalised - penalised - excess).abs() < 1e-9,
            "expected a penalty of {excess}, got {}",
            unpenalised - penalised
        );

        // A block within the maximum pays nothing.
        let within = segment_score(120.0, 0.0, SegmentKind::Commercial, true, &strict);
        let within_permissive =
            segment_score(120.0, 0.0, SegmentKind::Commercial, true, &permissive);
        assert!((within - within_permissive).abs() < 1e-9);
    }

    #[test]
    fn a_long_logo_bearing_stretch_is_programme_not_a_commercial_block() {
        let options = CutOptions::default();
        // Ten minutes with the logo up throughout: unambiguously programme.
        let as_commercial = segment_score(600.0, 1.0, SegmentKind::Commercial, true, &options);
        let as_programme = segment_score(600.0, 1.0, SegmentKind::Programme, true, &options);
        assert!(
            as_commercial < as_programme,
            "commercial {as_commercial} vs programme {as_programme}"
        );
    }
}
