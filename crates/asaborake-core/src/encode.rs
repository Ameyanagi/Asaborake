//! Building and running the encode.
//!
//! # How the cuts are applied
//!
//! Cutting by seeking and concatenating means one ffmpeg invocation per kept
//! range plus a concat pass, and every seek lands on a keyframe rather than
//! the frame that was asked for. Broadcast GOPs are half a second long, so
//! that is up to half a second of commercial left in, or half a second of
//! programme taken out, at every join.
//!
//! Asaborake instead selects frames inside a single filter chain:
//!
//! ```text
//! select='between(t,0,300)+between(t,360,900)',setpts=N/FRAME_RATE/TB
//! ```
//!
//! `select` passes only frames inside a kept range, and `setpts` renumbers
//! what survives into a continuous timeline. The decoder still decodes
//! everything, which costs time, but every cut lands on the exact frame and
//! the whole job is one pass.

use std::path::{Path, PathBuf};

use asaborake_cmcut::KeepRange;
use asaborake_media::{Chapter, Ffmpeg, MediaProbe, Progress};

use crate::Error;
use crate::profile::Profile;

/// Everything needed to encode one output file.
#[derive(Debug)]
pub struct EncodeRequest<'a> {
    /// Source recording.
    pub input: &'a Path,
    /// Where to write the result.
    pub output: &'a Path,
    /// How to encode it.
    pub profile: &'a Profile,
    /// Stretches of the source to keep, in order.
    pub keep: &'a [KeepRange],
    /// Chapters to write, already in output time.
    pub chapters: &'a [Chapter],
    /// What ffmpeg reported about the source.
    pub probe: &'a MediaProbe,
}

impl EncodeRequest<'_> {
    /// Duration of the output, in seconds.
    #[must_use]
    pub fn output_seconds(&self) -> f64 {
        self.keep.iter().map(KeepRange::duration).sum()
    }

    /// Whether any material is actually being removed.
    ///
    /// A single range covering the whole source means the filter chain can be
    /// left out entirely, which is both faster and less to go wrong.
    #[must_use]
    pub fn is_cutting(&self) -> bool {
        let source = self.probe.duration_seconds.unwrap_or(0.0);
        match self.keep {
            [] => false,
            [only] => only.start > 0.01 || (source > 0.0 && only.end < source - 0.01),
            _ => true,
        }
    }
}

/// Encode one output file, reporting progress as a fraction in `0.0..=1.0`.
///
/// # Errors
/// Returns [`Error::Media`] if ffmpeg fails, or [`Error::Io`] if the chapter
/// metadata cannot be written.
pub fn encode(
    ffmpeg: &Ffmpeg,
    request: &EncodeRequest<'_>,
    on_progress: &mut dyn FnMut(f64),
) -> Result<(), Error> {
    if !request.profile.is_supported_by(ffmpeg) {
        return Err(Error::UnsupportedProfile {
            profile: request.profile.name.clone(),
            encoder: request.profile.video.encoder.clone(),
        });
    }

    // The chapter file has to outlive the ffmpeg run, so it is kept in scope
    // for the whole function rather than being written inline.
    let chapter_file = write_chapters(request)?;
    let command = build_command(ffmpeg, request, chapter_file.as_deref());

    let total = request.output_seconds();
    asaborake_media::run_with_progress(command, |progress: Progress| {
        if total > 0.0 {
            on_progress((progress.out_time_seconds / total).clamp(0.0, 1.0));
        }
    })
    .map_err(Error::Media)?;

    Ok(())
}

/// Write the chapter metadata beside the output, returning its path.
fn write_chapters(request: &EncodeRequest<'_>) -> Result<Option<PathBuf>, Error> {
    if request.chapters.is_empty() {
        return Ok(None);
    }
    let path = request.output.with_extension("asaborake-chapters.txt");
    std::fs::write(&path, asaborake_media::ffmetadata(request.chapters)).map_err(|source| {
        Error::Io {
            path: path.clone(),
            source,
        }
    })?;
    Ok(Some(path))
}

/// Assemble the ffmpeg invocation.
fn build_command(
    ffmpeg: &Ffmpeg,
    request: &EncodeRequest<'_>,
    chapters: Option<&Path>,
) -> std::process::Command {
    let profile = request.profile;
    let interlaced = request.probe.video.as_ref().is_some_and(|v| v.interlaced);

    let mut command = ffmpeg.command();
    command.arg("-y");

    // Dual-mono decoding is an *input* option and must precede `-i`, or
    // ffmpeg silently ignores it and merges both languages together.
    if request.probe.is_dual_mono() {
        command.args(["-dual_mono_mode", &profile.audio.dual_mono_mode]);
    }
    // Terrestrial recordings routinely contain corrupt packets. Dropping them
    // beats aborting a job that is otherwise fine.
    command.args(["-fflags", "+discardcorrupt"]);
    command.arg("-i").arg(request.input);

    if let Some(path) = chapters {
        command.arg("-i").arg(path);
        // Take global metadata, and therefore the chapters, from input 1.
        command.args(["-map_metadata", "1"]);
    }

    let graph = filter_graph(request, interlaced);
    command.args(["-filter_complex", &graph]);
    command.args(["-map", "[v]", "-map", "[a]"]);

    command.arg("-c:v").arg(&profile.video.encoder);
    command.args(&profile.video.args);
    command.arg("-c:a").arg(&profile.audio.encoder);
    command.args(&profile.audio.args);
    command.args(["-ac", &profile.audio.channels.to_string()]);
    command.args(["-ar", &profile.audio.sample_rate.to_string()]);

    if profile.container == crate::profile::Container::Mp4 {
        // Put the index at the front so the file starts playing before it has
        // fully downloaded, which is how it will be watched.
        command.args(["-movflags", "+faststart"]);
    }
    command.args(["-f", profile.container.muxer()]);
    command.args(asaborake_media::progress_args());
    command.arg(request.output);

    command
}

/// Build the `-filter_complex` graph.
fn filter_graph(request: &EncodeRequest<'_>, interlaced: bool) -> String {
    let mut video: Vec<String> = request.profile.video_filters(interlaced);
    let mut audio: Vec<String> = Vec::new();

    if request.is_cutting() {
        let ranges = select_expression(request.keep);
        // Selection happens after deinterlacing so the filter sees whole
        // frames, and before scaling so no work is done on discarded frames.
        video.insert(
            usize::from(interlaced && request.profile.filters.deinterlace.is_some()),
            format!("select='{ranges}'"),
        );
        // Renumber the surviving frames into a continuous timeline; without
        // this the output keeps the source timestamps and every player sees a
        // file full of gaps.
        video.push("setpts=N/FRAME_RATE/TB".to_owned());

        audio.push(format!("aselect='{ranges}'"));
        audio.push("asetpts=N/SR/TB".to_owned());
    }

    // A chain must not be empty, and `null` is the documented no-op.
    if video.is_empty() {
        video.push("null".to_owned());
    }
    if audio.is_empty() {
        audio.push("anull".to_owned());
    }

    format!("[0:v]{}[v];[0:a]{}[a]", video.join(","), audio.join(","))
}

/// The `between(...)` expression selecting the kept ranges.
///
/// Commas inside the expression are escaped because a filtergraph reads an
/// unescaped comma as the end of one filter and the start of the next.
#[must_use]
pub fn select_expression(keep: &[KeepRange]) -> String {
    keep.iter()
        .map(|range| format!("between(t\\,{:.3}\\,{:.3})", range.start, range.end))
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::builtin;
    use asaborake_media::{MediaProbe, VideoStream};

    fn probe(interlaced: bool, duration: f64) -> MediaProbe {
        MediaProbe {
            duration_seconds: Some(duration),
            video: Some(VideoStream {
                index: 0,
                codec: "mpeg2video".into(),
                width: 1440,
                height: 1080,
                frame_rate: (30000, 1001),
                pixel_format: "yuv420p".into(),
                interlaced,
            }),
            audio: Vec::new(),
        }
    }

    fn request<'a>(
        profile: &'a Profile,
        keep: &'a [KeepRange],
        probe: &'a MediaProbe,
    ) -> EncodeRequest<'a> {
        EncodeRequest {
            input: Path::new("in.ts"),
            output: Path::new("out.mp4"),
            profile,
            keep,
            chapters: &[],
            probe,
        }
    }

    #[test]
    fn select_expression_escapes_its_commas() {
        let keep = [
            KeepRange {
                start: 0.0,
                end: 300.0,
            },
            KeepRange {
                start: 360.0,
                end: 900.5,
            },
        ];
        let expression = select_expression(&keep);
        assert_eq!(
            expression,
            "between(t\\,0.000\\,300.000)+between(t\\,360.000\\,900.500)"
        );
        assert!(
            !expression.contains(",t"),
            "an unescaped comma would truncate the filter"
        );
    }

    #[test]
    fn a_full_length_single_range_is_not_a_cut() {
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe(true, 600.0);
        let keep = [KeepRange {
            start: 0.0,
            end: 600.0,
        }];
        assert!(!request(&profile, &keep, &probe).is_cutting());

        let trimmed = [KeepRange {
            start: 0.0,
            end: 500.0,
        }];
        assert!(request(&profile, &trimmed, &probe).is_cutting());
    }

    #[test]
    fn the_graph_omits_selection_when_nothing_is_cut() {
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe(true, 600.0);
        let keep = [KeepRange {
            start: 0.0,
            end: 600.0,
        }];
        let graph = filter_graph(&request(&profile, &keep, &probe), true);

        assert!(!graph.contains("select"), "{graph}");
        assert!(graph.contains("bwdif"), "{graph}");
        assert!(graph.ends_with("[0:a]anull[a]"), "{graph}");
    }

    #[test]
    fn selection_runs_after_deinterlacing_and_before_scaling() {
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe(true, 600.0);
        let keep = [
            KeepRange {
                start: 0.0,
                end: 100.0,
            },
            KeepRange {
                start: 200.0,
                end: 300.0,
            },
        ];
        let graph = filter_graph(&request(&profile, &keep, &probe), true);

        let deinterlace = graph.find("bwdif").expect("deinterlace present");
        let select = graph.find("select=").expect("select present");
        let scale = graph.find("scale=").expect("scale present");
        assert!(
            deinterlace < select && select < scale,
            "unexpected order in {graph}"
        );
        assert!(graph.contains("setpts=N/FRAME_RATE/TB"), "{graph}");
        assert!(graph.contains("asetpts=N/SR/TB"), "{graph}");
    }

    #[test]
    fn a_progressive_source_puts_selection_first() {
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe(false, 600.0);
        let keep = [
            KeepRange {
                start: 0.0,
                end: 100.0,
            },
            KeepRange {
                start: 200.0,
                end: 300.0,
            },
        ];
        let graph = filter_graph(&request(&profile, &keep, &probe), false);
        assert!(graph.starts_with("[0:v]select="), "{graph}");
    }

    #[test]
    fn output_duration_is_the_sum_of_the_kept_ranges() {
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe(true, 1200.0);
        let keep = [
            KeepRange {
                start: 0.0,
                end: 300.0,
            },
            KeepRange {
                start: 400.0,
                end: 900.0,
            },
        ];
        assert!((request(&profile, &keep, &probe).output_seconds() - 800.0).abs() < 1e-9);
    }
}
