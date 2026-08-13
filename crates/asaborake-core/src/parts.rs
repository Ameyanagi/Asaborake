//! Splitting one recording into several output files.
//!
//! A video track has one picture size for its whole length. When a recording
//! changes size part-way through — a station dropping from HD to SD for a
//! segment, or a recording that spans a channel change — there is no single
//! file that can hold it. ffmpeg does not refuse; it scales everything to
//! whatever the first frame was, so the rest of the recording comes out
//! stretched, or it stops at the change and the remainder is silently missing.
//!
//! Amatsukaze splits the output instead, one file per run of constant
//! geometry, and so does this.

use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use asaborake_cmcut::KeepRange;

use crate::diagnostics::Diagnostics;

/// One output file, and the stretch of source it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// Where to write it.
    pub output: PathBuf,
    /// Where this part begins in the source, in seconds.
    pub start: f64,
    /// Where it ends in the source, in seconds.
    pub end: f64,
    /// The bytes of the source file this part occupies.
    ///
    /// `None` when the recording needs no splitting, which means the source
    /// file is used as it stands.
    pub bytes: Option<(u64, u64)>,
}

impl Part {
    /// How long this part is, in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// The kept ranges that fall inside this part, rebased to start at zero.
    ///
    /// A split part is cut out of the source as its own transport stream, so
    /// its clock starts at zero and a range at source second 700 is at part
    /// second 100 in a part beginning at 600.
    #[must_use]
    pub fn clip(&self, keep: &[KeepRange]) -> Vec<KeepRange> {
        keep.iter()
            .filter_map(|range| {
                let start = range.start.max(self.start);
                let end = range.end.min(self.end);
                // A range touching the boundary at a single point contributes
                // no frames and would produce an empty `between()` term.
                (end - start > 0.001).then_some(KeepRange {
                    start: start - self.start,
                    end: end - self.start,
                })
            })
            .collect()
    }

    /// Cut this part out of the source as a transport stream of its own.
    ///
    /// A transport stream is a sequence of fixed-size packets and both tables
    /// that describe it repeat continuously, so a byte range starting at a
    /// packet boundary is a complete, decodable stream. Nothing is re-encoded
    /// and nothing is parsed; the bytes are copied.
    ///
    /// Returns `None` when this part is the whole file, which needs no copy.
    ///
    /// # Errors
    /// Returns [`Error::Io`](crate::Error::Io) if the source cannot be read or
    /// the slice cannot be written.
    pub fn extract(&self, source: &Path, into: &Path) -> Result<Option<PathBuf>, crate::Error> {
        let Some((start, end)) = self.bytes else {
            return Ok(None);
        };

        let mut input = std::fs::File::open(source).map_err(|e| crate::Error::Io {
            path: source.to_path_buf(),
            source: e,
        })?;
        input
            .seek(std::io::SeekFrom::Start(start))
            .map_err(|e| crate::Error::Io {
                path: source.to_path_buf(),
                source: e,
            })?;

        let mut output = std::fs::File::create(into).map_err(|e| crate::Error::Io {
            path: into.to_path_buf(),
            source: e,
        })?;
        let mut remaining = end.saturating_sub(start);
        let mut buffer = vec![0u8; 1 << 20];
        while remaining > 0 {
            let want = remaining.min(buffer.len() as u64) as usize;
            let read = input
                .read(&mut buffer[..want])
                .map_err(|e| crate::Error::Io {
                    path: source.to_path_buf(),
                    source: e,
                })?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|e| crate::Error::Io {
                    path: into.to_path_buf(),
                    source: e,
                })?;
            remaining -= read as u64;
        }
        Ok(Some(into.to_path_buf()))
    }
}

/// Work out which files this recording has to become.
///
/// Returns exactly one part — the whole recording, written to `output` — when
/// the geometry never changes, which is every ordinary recording.
#[must_use]
pub fn split(
    output: &Path,
    diagnostics: Option<&Diagnostics>,
    duration: f64,
    file_size: u64,
) -> Vec<Part> {
    let whole = vec![Part {
        output: output.to_path_buf(),
        start: 0.0,
        end: duration.max(0.0),
        bytes: None,
    }];

    let Some(diagnostics) = diagnostics else {
        return whole;
    };
    // A point needs both a time and an offset to be usable: the time places
    // the cuts, the offset does the cutting.
    let mut points: Vec<(f64, u64)> = diagnostics
        .split_points
        .iter()
        .copied()
        .zip(diagnostics.split_offsets.iter().copied())
        .filter(|(at, offset)| {
            *at > 0.001 && *offset > 0 && (duration <= 0.0 || *at < duration - 0.001)
        })
        .collect();
    if points.is_empty() {
        return whole;
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Without a duration there is nothing to bound the last part with, so it
    // runs to wherever the source ends.
    let last = (
        if duration > 0.0 {
            duration
        } else {
            f64::INFINITY
        },
        file_size,
    );

    let mut parts = Vec::with_capacity(points.len() + 1);
    let mut start = (0.0, 0u64);
    for (index, at) in points.iter().chain(std::iter::once(&last)).enumerate() {
        parts.push(Part {
            output: numbered(output, index),
            start: start.0,
            end: at.0,
            bytes: Some((start.1, at.1)),
        });
        start = *at;
    }
    parts
}

/// The file name for part `index`, counting from zero.
///
/// The first part keeps the name that was asked for, so an ordinary recording
/// — and the first file of a split one — lands exactly where `EPGStation` is
/// expecting it. Only the extra files need a suffix, and they are the ones
/// nothing else knows about anyway.
fn numbered(output: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return output.to_path_buf();
    }
    let stem = output
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let extension = output
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    output.with_file_name(format!("{stem}.part{}{extension}", index + 1))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;

    /// Size of the pretend recording every test here splits.
    const FILE_SIZE: u64 = 1_800_000_000;

    /// Diagnostics whose split points carry a plausible byte offset each:
    /// a change a third of the way through a 1800-second recording sits about
    /// a third of the way through the file.
    fn diagnostics(split_points: Vec<f64>) -> Diagnostics {
        let split_offsets = split_points
            .iter()
            .map(|at| (at / 1800.0 * FILE_SIZE as f64) as u64)
            .collect();
        Diagnostics {
            duration_seconds: 1800.0,
            video: None,
            audio: Vec::new(),
            has_captions: false,
            format_changes: split_points.clone(),
            split_points,
            split_offsets,
            dropped_packets: 0,
            scrambled_packets: 0,
            error_packets: 0,
            total_packets: 1,
            dual_mono: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn an_ordinary_recording_is_one_file_under_the_name_that_was_asked_for() {
        let output = Path::new("/recordings/News.mp4");
        let parts = split(output, Some(&diagnostics(Vec::new())), 1800.0, FILE_SIZE);

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].output, output);
        assert_eq!(parts[0].start, 0.0);
        assert_eq!(parts[0].end, 1800.0);
    }

    #[test]
    fn a_recording_with_no_diagnostics_is_one_file() {
        // Not a transport stream, so nothing was scanned and nothing is known
        // about format changes. One file is the only safe reading.
        let parts = split(Path::new("/recordings/News.mp4"), None, 1800.0, FILE_SIZE);
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn a_size_change_becomes_a_second_file() {
        let output = Path::new("/recordings/News.mp4");
        let parts = split(output, Some(&diagnostics(vec![600.0])), 1800.0, FILE_SIZE);

        assert_eq!(parts.len(), 2);
        // The first keeps the name EPGStation is waiting for.
        assert_eq!(parts[0].output, output);
        assert_eq!(parts[0].start, 0.0);
        assert_eq!(parts[0].end, 600.0);
        assert_eq!(parts[1].output, PathBuf::from("/recordings/News.part2.mp4"));
        assert_eq!(parts[1].start, 600.0);
        assert_eq!(parts[1].end, 1800.0);
    }

    #[test]
    fn several_changes_become_several_files_in_order() {
        // A recording that goes HD, SD, HD needs a boundary at both changes.
        let parts = split(
            Path::new("/recordings/Show.mkv"),
            Some(&diagnostics(vec![1200.0, 400.0])),
            1800.0,
            FILE_SIZE,
        );

        assert_eq!(parts.len(), 3);
        let bounds: Vec<(f64, f64)> = parts.iter().map(|p| (p.start, p.end)).collect();
        assert_eq!(
            bounds,
            vec![(0.0, 400.0), (400.0, 1200.0), (1200.0, 1800.0)]
        );
        assert_eq!(parts[2].output, PathBuf::from("/recordings/Show.part3.mkv"));
    }

    #[test]
    fn a_change_outside_the_recording_is_ignored() {
        // A change at zero would make an empty first part, and one past the
        // end would make an empty last one.
        let parts = split(
            Path::new("/recordings/News.mp4"),
            Some(&diagnostics(vec![0.0, 1800.0, 5000.0])),
            1800.0,
            FILE_SIZE,
        );
        assert_eq!(parts.len(), 1, "{parts:?}");
    }

    #[test]
    fn kept_ranges_are_clipped_to_the_part_and_rebased_onto_its_own_clock() {
        let part = Part {
            output: PathBuf::from("x.mp4"),
            start: 600.0,
            end: 1200.0,
            bytes: None,
        };
        let keep = [
            // Entirely before this part.
            KeepRange {
                start: 0.0,
                end: 300.0,
            },
            // Straddling its start.
            KeepRange {
                start: 500.0,
                end: 700.0,
            },
            // Wholly inside.
            KeepRange {
                start: 800.0,
                end: 900.0,
            },
            // Straddling its end.
            KeepRange {
                start: 1100.0,
                end: 1500.0,
            },
        ];

        let clipped = part.clip(&keep);
        assert_eq!(
            clipped,
            vec![
                KeepRange {
                    start: 0.0,
                    end: 100.0
                },
                KeepRange {
                    start: 200.0,
                    end: 300.0
                },
                KeepRange {
                    start: 500.0,
                    end: 600.0
                },
            ]
        );
    }

    #[test]
    fn a_part_containing_nothing_kept_yields_no_ranges() {
        // Every commercial: the part exists but there is nothing to write.
        let part = Part {
            output: PathBuf::from("x.mp4"),
            start: 600.0,
            end: 1200.0,
            bytes: None,
        };
        let keep = [KeepRange {
            start: 0.0,
            end: 300.0,
        }];
        assert!(part.clip(&keep).is_empty());
    }

    #[test]
    fn a_range_touching_a_boundary_at_one_point_is_dropped() {
        // It contains no frames, and an empty `between()` term in the filter
        // would select nothing while still costing a term.
        let part = Part {
            output: PathBuf::from("x.mp4"),
            start: 600.0,
            end: 1200.0,
            bytes: None,
        };
        let keep = [KeepRange {
            start: 300.0,
            end: 600.0,
        }];
        assert!(part.clip(&keep).is_empty());
    }

    #[test]
    fn a_byte_range_is_copied_out_verbatim() {
        // Nothing is decoded or re-encoded; a transport stream cut at a packet
        // boundary is already a transport stream.
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("whole.ts");
        let slice = dir.path().join("part.ts");
        let bytes: Vec<u8> = (0..=255u8).cycle().take(4000).collect();
        std::fs::write(&source, &bytes).expect("writes");

        let part = Part {
            output: PathBuf::from("out.mp4"),
            start: 10.0,
            end: 20.0,
            bytes: Some((1880, 3760)),
        };
        let written = part
            .extract(&source, &slice)
            .expect("extracts")
            .expect("a slice");

        assert_eq!(written, slice);
        assert_eq!(std::fs::read(&slice).expect("reads"), bytes[1880..3760]);
    }

    #[test]
    fn a_part_covering_the_whole_file_is_not_copied() {
        // The common case: no split, so the source is used where it lies.
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("whole.ts");
        std::fs::write(&source, b"x").expect("writes");

        let part = Part {
            output: PathBuf::from("out.mp4"),
            start: 0.0,
            end: 10.0,
            bytes: None,
        };
        assert_eq!(
            part.extract(&source, &dir.path().join("part.ts"))
                .expect("extracts"),
            None
        );
    }

    #[test]
    fn a_file_without_an_extension_still_gets_numbered() {
        assert_eq!(
            numbered(Path::new("/recordings/News"), 1),
            PathBuf::from("/recordings/News.part2")
        );
    }
}
