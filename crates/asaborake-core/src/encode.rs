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

/// How the audio streams are carried into the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlan {
    /// Copied byte for byte.
    ///
    /// Broadcast audio is already AAC, which is what the output container
    /// wants, so re-encoding it only loses quality and spends time. This is
    /// possible whenever nothing is being cut, because the stream is then
    /// passed through untouched.
    Copy,
    /// Decoded, selected and re-encoded.
    ///
    /// Required when material is being removed: the cut has to fall inside the
    /// audio stream, and a coded stream cannot be cut at an arbitrary point
    /// without decoding it.
    Reencode,
}

impl EncodeRequest<'_> {
    /// Duration of the output, in seconds.
    #[must_use]
    pub fn output_seconds(&self) -> f64 {
        self.keep.iter().map(KeepRange::duration).sum()
    }

    /// How many audio streams the source carries.
    ///
    /// Every one becomes a track. Japanese broadcast routinely carries two —
    /// a bilingual programme's main and sub language — and mapping only the
    /// first silently discards the second.
    #[must_use]
    pub fn audio_streams(&self) -> usize {
        self.probe.audio.len().max(1)
    }

    /// Whether the audio can be copied rather than re-encoded.
    #[must_use]
    pub fn audio_plan(&self) -> AudioPlan {
        if self.is_cutting() {
            return AudioPlan::Reencode;
        }
        // A container that cannot carry the source codec forces a re-encode
        // even when nothing is being cut.
        let carried = self
            .probe
            .audio
            .iter()
            .all(|stream| self.profile.container.can_carry(&stream.codec));
        if carried && !self.probe.audio.is_empty() {
            AudioPlan::Copy
        } else {
            AudioPlan::Reencode
        }
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

    let plan = request.audio_plan();
    let streams = request.audio_streams();

    let graph = filter_graph(request, interlaced);
    command.args(["-filter_complex", &graph]);
    command.args(["-map", "[v]"]);

    match plan {
        AudioPlan::Copy => {
            // Straight from the source, one map per stream, so a bilingual
            // programme keeps both languages.
            for index in 0..streams {
                command.args(["-map", &format!("0:a:{index}?")]);
            }
        }
        AudioPlan::Reencode => {
            for index in 0..streams {
                command.args(["-map", &format!("[a{index}]")]);
            }
        }
    }

    command.arg("-c:v").arg(&profile.video.encoder);
    command.args(&profile.video.args);

    match plan {
        AudioPlan::Copy => {
            command.args(["-c:a", "copy"]);
        }
        AudioPlan::Reencode => {
            command.arg("-c:a").arg(&profile.audio.encoder);
            command.args(&profile.audio.args);
            // Channel and rate conversion only make sense when decoding; with
            // a copy they would contradict the stream being copied.
            command.args(["-ac", &profile.audio.channels.to_string()]);
            command.args(["-ar", &profile.audio.sample_rate.to_string()]);
        }
    }

    // Carry each track's language through, so a player can offer the choice.
    for (index, stream) in request.probe.audio.iter().enumerate() {
        if let Some(language) = stream.language.as_deref() {
            command.args([
                &format!("-metadata:s:a:{index}"),
                &format!("language={language}"),
            ]);
        }
    }

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
///
/// The video always goes through a chain. The audio only does when it is being
/// re-encoded — a copied stream never enters the graph, because passing it
/// through a filter would mean decoding it, which is the thing being avoided.
fn filter_graph(request: &EncodeRequest<'_>, interlaced: bool) -> String {
    let mut video: Vec<String> = request.profile.video_filters(interlaced);
    let mut audio: Vec<String> = Vec::new();

    if request.is_cutting() {
        let ranges = select_expression(request.keep);

        // Selection happens after deinterlacing so the filter sees whole
        // frames, and before scaling so no work is done on frames that are
        // about to be discarded.
        let at = usize::from(interlaced && request.profile.filters.deinterlace.is_some());

        // The timeline is rebuilt from the frame index *before* selecting, so
        // that `t` here means the same thing it meant during analysis.
        //
        // The analysis derives every position by counting decoded frames, not
        // by reading container timestamps. A source whose timestamps start at
        // an offset — which broadcast recordings routinely do — would
        // otherwise be cut a constant distance away from where the analysis
        // asked. For a well-behaved constant-rate source this is a no-op.
        video.insert(at, "setpts=N/FRAME_RATE/TB".to_owned());
        video.insert(at + 1, format!("select='{ranges}'"));

        // And renumber again afterwards, so what survives is contiguous rather
        // than a timeline full of holes.
        video.push("setpts=N/FRAME_RATE/TB".to_owned());

        audio.push("asetpts=N/SR/TB".to_owned());
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

    let mut chains = vec![format!("[0:v]{}[v]", video.join(","))];

    // One chain per audio stream when re-encoding; none at all when copying,
    // since a copied stream is mapped straight from the input.
    if request.audio_plan() == AudioPlan::Reencode {
        let joined = audio.join(",");
        for index in 0..request.audio_streams() {
            chains.push(format!("[0:a:{index}]{joined}[a{index}]"));
        }
    }

    chains.join(";")
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
        probe_with_audio(interlaced, duration, 1)
    }

    /// A probe carrying `tracks` AAC audio streams, as broadcast does.
    fn probe_with_audio(interlaced: bool, duration: f64, tracks: usize) -> MediaProbe {
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
            audio: (0..tracks)
                .map(|index| asaborake_media::AudioStream {
                    index: index as u32 + 1,
                    codec: "aac".into(),
                    channels: 2,
                    sample_rate: 48_000,
                    language: Some(if index == 0 {
                        "jpn".into()
                    } else {
                        "eng".into()
                    }),
                })
                .collect(),
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
        // And no audio chain at all: with nothing to cut the streams are
        // copied, and a copied stream must not be decoded.
        assert!(!graph.contains("[0:a"), "{graph}");
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

        // The timeline must be rebuilt from the frame index before selecting,
        // or the selection reads container timestamps that a concatenated
        // recording restarts part-way through.
        let normalise = graph
            .find("setpts=N/FRAME_RATE/TB")
            .expect("setpts present");
        assert!(normalise < select, "setpts must precede select in {graph}");
        assert_eq!(
            graph.matches("setpts=N/FRAME_RATE/TB").count(),
            2,
            "once before the cut and once after: {graph}"
        );
        assert_eq!(graph.matches("asetpts=N/SR/TB").count(), 2, "{graph}");
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
        assert!(
            graph.starts_with("[0:v]setpts=N/FRAME_RATE/TB,select="),
            "{graph}"
        );
    }

    #[test]
    fn audio_is_copied_when_nothing_is_being_cut() {
        // Broadcast audio is already AAC, which is what the container wants.
        // Re-encoding it only loses quality and spends time.
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe_with_audio(true, 600.0, 2);
        let keep = [KeepRange {
            start: 0.0,
            end: 600.0,
        }];
        assert_eq!(
            request(&profile, &keep, &probe).audio_plan(),
            AudioPlan::Copy
        );
    }

    #[test]
    fn audio_is_re_encoded_when_material_is_removed() {
        // A coded stream cannot be cut at an arbitrary point without decoding.
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe_with_audio(true, 600.0, 2);
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
        assert_eq!(
            request(&profile, &keep, &probe).audio_plan(),
            AudioPlan::Reencode
        );
    }

    #[test]
    fn every_audio_stream_gets_its_own_chain_when_re_encoding() {
        // A bilingual programme carries two streams; mapping only the first
        // silently discards a language.
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe_with_audio(true, 600.0, 2);
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

        assert!(graph.contains("[0:a:0]"), "{graph}");
        assert!(graph.contains("[a0]"), "{graph}");
        assert!(graph.contains("[0:a:1]"), "{graph}");
        assert!(graph.contains("[a1]"), "{graph}");
        assert_eq!(
            graph.matches("aselect=").count(),
            2,
            "one per stream: {graph}"
        );
    }

    #[test]
    fn a_copied_stream_never_enters_the_filter_graph() {
        // Passing it through a filter would mean decoding it, which is the
        // thing being avoided.
        let profile = builtin().remove("x264-cpu").expect("profile");
        let probe = probe_with_audio(true, 600.0, 2);
        let keep = [KeepRange {
            start: 0.0,
            end: 600.0,
        }];
        let graph = filter_graph(&request(&profile, &keep, &probe), true);

        assert!(!graph.contains("0:a"), "{graph}");
        assert!(!graph.contains("anull"), "{graph}");
        assert!(
            graph.contains("[0:v]"),
            "the video still needs one: {graph}"
        );
    }

    #[test]
    fn a_container_that_cannot_carry_the_codec_forces_a_re_encode() {
        use crate::profile::Container;
        assert!(Container::Mp4.can_carry("aac"));
        assert!(!Container::Mp4.can_carry("pcm_s16le"));
        // Matroska takes essentially anything.
        assert!(Container::Mkv.can_carry("pcm_s16le"));
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
