//! Turning a cut plan into chapters the player can navigate.
//!
//! Chapters are written either way, and they mean different things:
//!
//! - when the commercials were **cut**, they mark where each part of the
//!   programme resumes, so a viewer can jump between acts;
//! - when they were **kept** — because detection was not confident — they mark
//!   the commercials themselves, so the viewer can skip what Asaborake would
//!   have removed, and judge whether it was right.
//!
//! The second case is why the low-confidence path is useful rather than merely
//! safe: it degrades to a manual version of the same result.

use asaborake_cmcut::{Decision, KeepRange, Segment, SegmentKind};
use asaborake_media::Chapter;

/// Map a source timestamp onto the output timeline.
///
/// Returns `None` for a time that falls inside a removed stretch, which has no
/// position in the output at all.
#[must_use]
pub fn source_to_output(keep: &[KeepRange], seconds: f64) -> Option<f64> {
    let mut elapsed = 0.0;
    for range in keep {
        if seconds < range.start {
            return None;
        }
        if seconds <= range.end {
            return Some(elapsed + (seconds - range.start));
        }
        elapsed += range.duration();
    }
    None
}

/// Build the chapter list for a plan.
#[must_use]
pub fn chapters_for(segments: &[Segment], keep: &[KeepRange], decision: Decision) -> Vec<Chapter> {
    match decision {
        // Nothing was removed, so source time is output time and every
        // segment becomes a chapter the viewer can skip between.
        Decision::KeepAll => {
            let mut programme = 0;
            let mut commercial = 0;
            segments
                .iter()
                .map(|segment| {
                    let title = match segment.kind {
                        SegmentKind::Programme => {
                            programme += 1;
                            format!("Part {programme}")
                        }
                        SegmentKind::Commercial => {
                            commercial += 1;
                            format!("CM {commercial}")
                        }
                    };
                    Chapter {
                        start_seconds: segment.start,
                        end_seconds: segment.end,
                        title,
                    }
                })
                .collect()
        }
        Decision::Cut => {
            let mut part = 0;
            segments
                .iter()
                .filter(|segment| segment.kind == SegmentKind::Programme)
                .filter_map(|segment| {
                    let start = source_to_output(keep, segment.start)?;
                    let end = source_to_output(keep, segment.end)?;
                    part += 1;
                    Some(Chapter {
                        start_seconds: start,
                        end_seconds: end,
                        title: format!("Part {part}"),
                    })
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments() -> Vec<Segment> {
        vec![
            Segment {
                start: 0.0,
                end: 300.0,
                kind: SegmentKind::Programme,
                confidence: 0.9,
            },
            Segment {
                start: 300.0,
                end: 390.0,
                kind: SegmentKind::Commercial,
                confidence: 0.9,
            },
            Segment {
                start: 390.0,
                end: 900.0,
                kind: SegmentKind::Programme,
                confidence: 0.9,
            },
        ]
    }

    fn keep() -> Vec<KeepRange> {
        vec![
            KeepRange {
                start: 0.0,
                end: 300.0,
            },
            KeepRange {
                start: 390.0,
                end: 900.0,
            },
        ]
    }

    #[test]
    fn source_times_map_onto_a_gapless_output_timeline() {
        let keep = keep();
        assert_eq!(source_to_output(&keep, 0.0), Some(0.0));
        assert_eq!(source_to_output(&keep, 150.0), Some(150.0));
        // The instant the second kept range starts is 300s into the output,
        // because the 90-second break vanished.
        assert_eq!(source_to_output(&keep, 390.0), Some(300.0));
        assert_eq!(source_to_output(&keep, 900.0), Some(810.0));
    }

    #[test]
    fn a_time_inside_a_removed_stretch_has_no_output_position() {
        let keep = keep();
        assert_eq!(source_to_output(&keep, 340.0), None);
        assert_eq!(source_to_output(&keep, 1000.0), None);
    }

    #[test]
    fn cutting_yields_one_chapter_per_surviving_part() {
        let chapters = chapters_for(&segments(), &keep(), Decision::Cut);
        assert_eq!(chapters.len(), 2);

        assert_eq!(chapters[0].title, "Part 1");
        assert!((chapters[0].start_seconds - 0.0).abs() < 1e-9);
        assert!((chapters[0].end_seconds - 300.0).abs() < 1e-9);

        assert_eq!(chapters[1].title, "Part 2");
        assert!((chapters[1].start_seconds - 300.0).abs() < 1e-9);
        assert!((chapters[1].end_seconds - 810.0).abs() < 1e-9);
    }

    #[test]
    fn chapters_are_contiguous_after_cutting() {
        let chapters = chapters_for(&segments(), &keep(), Decision::Cut);
        for pair in chapters.windows(2) {
            assert!(
                (pair[1].start_seconds - pair[0].end_seconds).abs() < 1e-6,
                "gap between {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn keeping_everything_marks_the_commercials_so_they_can_be_skipped() {
        let whole = vec![KeepRange {
            start: 0.0,
            end: 900.0,
        }];
        let chapters = chapters_for(&segments(), &whole, Decision::KeepAll);

        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].title, "Part 1");
        assert_eq!(chapters[1].title, "CM 1");
        assert_eq!(chapters[2].title, "Part 2");
        // Source time is output time when nothing was removed.
        assert!((chapters[1].start_seconds - 300.0).abs() < 1e-9);
        assert!((chapters[1].end_seconds - 390.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_plan_yields_no_chapters() {
        assert!(chapters_for(&[], &[], Decision::Cut).is_empty());
        assert!(chapters_for(&[], &[], Decision::KeepAll).is_empty());
    }
}
