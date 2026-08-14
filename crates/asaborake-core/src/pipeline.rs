//! The job pipeline: one recording in, one transcoded file out.
//!
//! ```text
//! probe -> analyse -> plan the cuts -> encode -> write the sidecar
//! ```
//!
//! Progress from every phase is folded onto a single fraction, because that is
//! what `EPGStation` and the web UI both display. Analysis gets the first third
//! and encoding the rest, which roughly matches how the time divides in
//! practice once a channel's logo is in the store.

use std::path::{Path, PathBuf};

use asaborake_analyze::{Analysis, AnalysisOptions, AnalysisProgress, Stage};
use asaborake_cmcut::{CutOptions, CutPlan, Decision, LowConfidencePolicy};
use asaborake_media::Ffmpeg;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::diagnostics::Diagnostics;
use crate::encode::{EncodeRequest, encode};
use crate::profile::Profile;
use crate::store::LogoStore;

/// Fraction of the progress bar given to analysis.
///
/// With a stored logo, analysis is one decode and encoding is one encode, so
/// the split is roughly even; without one, analysis takes three extra passes
/// and overruns its share. Reporting a bar that stalls is better than one that
/// jumps backwards, so the split is fixed.
const ANALYSIS_SHARE: f64 = 0.35;

/// What to do with one recording.
#[derive(Debug, Clone)]
pub struct JobRequest {
    /// Source recording.
    pub input: PathBuf,
    /// Where to write the result.
    pub output: PathBuf,
    /// How to encode it.
    pub profile: Profile,
    /// Channel the recording came from, used as the logo store key.
    pub channel_id: Option<String>,
    /// Human-readable channel name, used to name a newly learned logo.
    pub channel_name: Option<String>,
    /// Programme title, for logging and the web UI.
    pub title: Option<String>,
    /// Where the logo is, when it is already known.
    pub logo_rect: Option<asaborake_analyze::Rect>,
    /// Segmentation tunables.
    pub cut: CutOptions,
    /// Whether to learn and store a logo when the channel has none.
    pub learn_logo: bool,
    /// Whether to refuse a recording the transport-stream scan calls hopeless.
    ///
    /// Off by default: a scan can be wrong about an unusual recording, and
    /// producing a poor file is a better failure than producing none.
    pub refuse_damaged: bool,
    /// Cuts chosen by hand, which replace the segmenter's answer entirely.
    ///
    /// A detection that got it wrong is a lost recording only if there is no
    /// way to correct it. When these are given the analysis still runs — the
    /// timeline is drawn from it — but nothing it concludes is acted on.
    pub manual_ranges: Option<Vec<asaborake_cmcut::KeepRange>>,
    /// What the source contains, when the caller has already scanned it.
    ///
    /// A caller that records this before starting — as the server does — keeps
    /// it on a job that then fails, which is the case where knowing the
    /// recording was damaged explains the failure. Left unset, the pipeline
    /// scans the source itself.
    pub diagnostics: Option<Diagnostics>,
}

impl JobRequest {
    /// A request with sensible defaults for a recording.
    #[must_use]
    pub fn new(input: impl Into<PathBuf>, output: impl Into<PathBuf>, profile: Profile) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
            profile,
            channel_id: None,
            channel_name: None,
            title: None,
            logo_rect: None,
            cut: CutOptions::default(),
            learn_logo: true,
            refuse_damaged: false,
            manual_ranges: None,
            diagnostics: None,
        }
    }
}

/// What the pipeline produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobOutcome {
    /// The file that was asked for, and the first one written.
    pub output: PathBuf,
    /// Every file written, in order.
    ///
    /// More than one when the recording changed picture size part-way through
    /// and had to be split; `EPGStation` only knows about the first, which is
    /// why that one keeps the name it asked for.
    #[serde(default)]
    pub outputs: Vec<PathBuf>,
    /// The sidecar describing the cuts.
    pub sidecar: PathBuf,
    /// What analysis found.
    pub analysis: Analysis,
    /// What was decided.
    pub plan: CutPlan,
    /// Whether a logo was learned and added to the store.
    pub logo_learned: bool,
    /// What the recording contained and what was wrong with it, when the
    /// source was a transport stream that could be scanned.
    pub diagnostics: Option<Diagnostics>,
}

/// The record written beside the output.
///
/// This is what makes a cut reviewable after the fact: it says what was
/// removed, why, and how confident the segmenter was, so a surprising result
/// can be understood without re-running anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    /// Asaborake version that produced it.
    pub version: String,
    /// Source recording.
    pub input: PathBuf,
    /// Encoding profile used.
    pub profile: String,
    /// Programme title, when known.
    pub title: Option<String>,
    /// What was decided.
    pub plan: CutPlan,
    /// What analysis found, minus the per-frame track, which is large.
    pub analysis: Analysis,
    /// What the source recording contained and what was wrong with it.
    ///
    /// Optional so a sidecar written before this existed still parses.
    #[serde(default)]
    pub diagnostics: Option<Diagnostics>,
}

/// Progress from somewhere in the pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineProgress {
    /// Overall completion, in `0.0..=1.0`.
    pub fraction: f64,
    /// What is happening, in a form suitable for a log line.
    pub message: String,
}

/// Run one job.
///
/// # Errors
/// Returns [`Error::Analyze`] if analysis fails, [`Error::Media`] if ffmpeg
/// fails, [`Error::LowConfidence`] if the plan was untrustworthy and the
/// policy is to fail, or [`Error::Io`] on a filesystem failure.
pub fn run(
    ffmpeg: &Ffmpeg,
    store: Option<&LogoStore>,
    request: &JobRequest,
    on_progress: &mut dyn FnMut(PipelineProgress),
) -> Result<JobOutcome, Error> {
    if !request.profile.is_supported_by(ffmpeg) {
        return Err(Error::UnsupportedProfile {
            profile: request.profile.name.clone(),
            encoder: request.profile.video.encoder.clone(),
        });
    }

    let probe = asaborake_media::probe(ffmpeg, &request.input).map_err(Error::Media)?;
    let video = probe.video.as_ref().ok_or_else(|| {
        Error::Media(asaborake_media::Error::NoVideoStream {
            path: request.input.clone(),
        })
    })?;

    // Scanned by the caller when it wanted the source recorded before the work
    // began, which is what keeps the diagnostics on a job that then fails.
    let diagnostics = match request.diagnostics.clone() {
        Some(diagnostics) => Some(diagnostics),
        None => inspect(&request.input),
    };
    report(diagnostics.as_ref(), request.refuse_damaged)?;

    // A channel known to carry no logo — NHK's, the shopping channels, most
    // of CS — must not be searched. Looking anyway costs three extra decoding
    // passes per recording to rediscover the same nothing, and risks the
    // locator settling on a telop banner instead.
    let no_logo = store
        .zip(request.channel_id.as_deref())
        .is_some_and(|(store, channel)| store.has_no_logo(channel));
    if no_logo {
        tracing::info!("this channel is marked as having no logo; not looking for one");
    }

    // A stored logo turns three extra decoding passes into none.
    let stored = store
        .zip(request.channel_id.as_deref())
        .and_then(|(store, channel)| store.load(channel, video.width, video.height));
    if stored.is_some() {
        tracing::info!("using a stored logo for this channel");
    }

    let options = AnalysisOptions {
        logo: stored.clone(),
        logo_name: request
            .channel_name
            .clone()
            .or_else(|| request.channel_id.clone())
            .unwrap_or_else(|| "unknown".to_owned()),
        channel_id: request.channel_id.clone(),
        logo_rect: request.logo_rect,
        deinterlace: video.interlaced,
        // A channel that carries no commercials has nothing for a logo to
        // separate, so searching for one is a decoding pass spent on a
        // question nobody asked.
        find_logo: !no_logo && request.cut.detect,
        ..AnalysisOptions::default()
    };

    let analysis = asaborake_analyze::analyse(
        ffmpeg,
        &request.input,
        &options,
        &mut |progress: AnalysisProgress| {
            on_progress(PipelineProgress {
                fraction: progress.fraction * ANALYSIS_SHARE,
                message: describe(progress.stage),
            });
        },
    )
    .map_err(Error::Analyze)?;

    let logo_learned = request.learn_logo && !no_logo && remember_logo(store, &analysis);

    let plan = decide(&analysis, request);
    tracing::info!(
        confidence = plan.confidence,
        removed_seconds = plan.removed_seconds(),
        commercial_seconds = plan.cut_seconds(),
        reason = %plan.reason,
        "cut plan"
    );

    check_confidence(&plan, request, &analysis, no_logo)?;

    let outputs = encode_parts(
        ffmpeg,
        request,
        &probe,
        &plan,
        diagnostics.as_ref(),
        analysis.duration_seconds,
        on_progress,
    )?;

    write_captions(
        request,
        &parts_of(request, diagnostics.as_ref(), &analysis),
        &plan,
    );

    let sidecar = write_sidecar(request, &analysis, &plan, diagnostics.clone())?;

    on_progress(PipelineProgress {
        fraction: 1.0,
        message: "done".to_owned(),
    });

    Ok(JobOutcome {
        output: request.output.clone(),
        outputs,
        sidecar,
        analysis,
        plan,
        logo_learned,
        diagnostics,
    })
}

/// The parts this recording becomes, for anything that needs them twice.
fn parts_of(
    request: &JobRequest,
    diagnostics: Option<&Diagnostics>,
    analysis: &Analysis,
) -> Vec<crate::parts::Part> {
    let file_size = std::fs::metadata(&request.input).map_or(0, |m| m.len());
    crate::parts::split(
        &request.output,
        diagnostics,
        analysis.duration_seconds,
        file_size,
    )
}

/// Write the recording's captions beside each output file.
///
/// Re-timed through the same map the chapters use, because a subtitle that
/// still speaks in source time drifts further out of step with every
/// commercial removed — by the end of a programme, minutes.
///
/// A failure here is logged and otherwise ignored. Captions are worth having
/// and are not worth failing a transcode for.
fn write_captions(request: &JobRequest, parts: &[crate::parts::Part], plan: &CutPlan) {
    let file = match std::fs::File::open(&request.input) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, "could not reopen the source to read its captions");
            return;
        }
    };
    let captions = match asaborake_ts::caption::extract(std::io::BufReader::new(file)) {
        Ok(captions) if !captions.is_empty() => captions,
        Ok(_) => return,
        Err(error) => {
            tracing::warn!(%error, "could not read the captions");
            return;
        }
    };

    for part in parts {
        let keep = part.clip(&plan.keep);
        if keep.is_empty() {
            continue;
        }
        // Each part has a clock of its own, so a caption is placed against
        // that part's kept ranges or not at all.
        let mut retimed = Vec::new();
        for caption in &captions {
            let start = caption.start_seconds - part.start;
            let end = caption.end_seconds - part.start;
            let Some(mapped_start) = crate::chapters::source_to_output(&keep, start) else {
                // It fell inside something that was cut out, so there is no
                // moment in the output for it to be shown at.
                continue;
            };
            let mapped_end = crate::chapters::source_to_output(&keep, end)
                .unwrap_or(mapped_start + (end - start));
            retimed.push(asaborake_ts::Caption {
                start_seconds: mapped_start,
                end_seconds: mapped_end.max(mapped_start + 0.1),
                text: caption.text.clone(),
            });
        }
        if retimed.is_empty() {
            continue;
        }

        let path = part.output.with_extension("srt");
        match std::fs::write(&path, asaborake_ts::to_srt(&retimed)) {
            Ok(()) => tracing::info!(
                path = %path.display(),
                captions = retimed.len(),
                "wrote subtitles"
            ),
            Err(error) => tracing::warn!(%error, "could not write the subtitles"),
        }
    }
}

/// Encode the recording, as one file or as several.
///
/// A video track has one picture size for its whole length, so a recording
/// that changes size part-way through becomes more than one file. Every
/// ordinary recording is a single part and takes the same path.
fn encode_parts(
    ffmpeg: &Ffmpeg,
    request: &JobRequest,
    probe: &asaborake_media::MediaProbe,
    plan: &CutPlan,
    diagnostics: Option<&Diagnostics>,
    duration: f64,
    on_progress: &mut dyn FnMut(PipelineProgress),
) -> Result<Vec<PathBuf>, Error> {
    let parts = crate::parts::split(
        &request.output,
        diagnostics,
        duration,
        std::fs::metadata(&request.input).map_or(0, |m| m.len()),
    );
    if parts.len() > 1 {
        tracing::info!(
            parts = parts.len(),
            "the picture size changes mid-recording; writing one file per size"
        );
    }

    // Each part gets its own slice of what is left of the progress bar, so the
    // bar advances once across the whole job rather than restarting per file.
    let share = (1.0 - ANALYSIS_SHARE) / parts.len() as f64;

    let mut outputs = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let keep = part.clip(&plan.keep);
        if keep.is_empty() {
            // Every second of this part was judged a commercial. Writing an
            // empty file would be worse than writing none.
            tracing::info!(part = index + 1, "nothing to keep in this part; skipping");
            continue;
        }
        let segments = clip_segments(&plan.segments, part);
        let chapters = crate::chapters::chapters_for(&segments, &keep, plan.decision);

        let base = ANALYSIS_SHARE + share * index as f64;
        let label = if parts.len() > 1 {
            format!("encoding part {} of {}", index + 1, parts.len())
        } else {
            "encoding".to_owned()
        };

        // A part of a split recording is cut out of the source as its own
        // transport stream first. Encoding it in place is not possible: the
        // filter graph reinitialises at the picture-size change and restarts
        // the frame counter its clock comes from, so no expression over time
        // can name the far side of the change.
        let slice = part.extract(&request.input, &part.output.with_extension("part.ts"))?;
        let source = slice.as_deref().unwrap_or(&request.input);

        // The slice is its own file, so ffmpeg's view of it — its duration
        // above all — has to be taken from the slice rather than the whole.
        let sliced_probe = match slice {
            Some(_) => Some(asaborake_media::probe(ffmpeg, source).map_err(Error::Media)?),
            None => None,
        };
        let probe = sliced_probe.as_ref().unwrap_or(probe);

        let result = encode(
            ffmpeg,
            &EncodeRequest {
                input: source,
                output: &part.output,
                profile: &request.profile,
                keep: &keep,
                chapters: &chapters,
                probe,
                dual_mono: diagnostics.and_then(|d| d.dual_mono.as_ref()),
            },
            &mut |fraction| {
                on_progress(PipelineProgress {
                    fraction: base + fraction * share,
                    message: label.clone(),
                });
            },
        );

        // The slice is scratch space; it is the size of the part it holds and
        // must not be left behind whether the encode worked or not.
        if let Some(slice) = &slice {
            let _ = std::fs::remove_file(slice);
        }
        result?;
        outputs.push(part.output.clone());
    }
    Ok(outputs)
}

/// The segments falling inside one part, rebased to it.
///
/// Chapters are built from segments against the kept ranges, and a part cut
/// out of the source has a clock of its own starting at zero.
fn clip_segments(
    segments: &[asaborake_cmcut::Segment],
    part: &crate::parts::Part,
) -> Vec<asaborake_cmcut::Segment> {
    segments
        .iter()
        .filter_map(|segment| {
            let start = segment.start.max(part.start);
            let end = segment.end.min(part.end);
            (end - start > 0.001).then_some(asaborake_cmcut::Segment {
                start: start - part.start,
                end: end - part.start,
                ..*segment
            })
        })
        .collect()
}

/// What to cut: either what somebody chose, or what the segmenter worked out.
///
/// Hand-chosen ranges replace the answer entirely rather than nudging it. The
/// analysis still runs, because the timeline is drawn from it, but nothing it
/// concludes is acted on — the person looking at that timeline has already
/// overruled it.
fn decide(analysis: &Analysis, request: &JobRequest) -> CutPlan {
    let Some(keep) = &request.manual_ranges else {
        return asaborake_cmcut::plan(analysis, &request.cut);
    };
    tracing::info!(ranges = keep.len(), "using cuts chosen by hand");
    CutPlan {
        // No segments: nobody labelled the gaps, they simply are not kept.
        segments: Vec::new(),
        keep: keep.clone(),
        // Somebody looked at it, which is the most confidence available.
        confidence: 1.0,
        decision: Decision::Cut,
        reason: "cut by hand".to_owned(),
    }
}

/// Stop the job when the plan is not trustworthy and the policy says to.
///
/// # Errors
/// Returns [`Error::LowConfidence`] or [`Error::NeedsLogo`] accordingly.
fn check_confidence(
    plan: &CutPlan,
    request: &JobRequest,
    analysis: &Analysis,
    no_logo: bool,
) -> Result<(), Error> {
    if plan.decision != Decision::KeepAll || plan.confidence >= request.cut.confidence_threshold {
        return Ok(());
    }
    match request.cut.low_confidence {
        LowConfidencePolicy::Fail => Err(Error::LowConfidence {
            reason: plan.reason.clone(),
        }),
        // Stopping before the encode is the whole point: an hour of GPU time
        // spent producing an uncut recording is an hour spent producing
        // something that will have to be done again. A channel *known* to have
        // no logo is the exception — there is nothing to wait for, so waiting
        // would hold it for ever.
        LowConfidencePolicy::Block if !analysis.has_logo() && !no_logo => Err(Error::NeedsLogo),
        _ => Ok(()),
    }
}

/// Cache a freshly learned logo, so the next recording on this channel skips
/// the learning passes entirely. Returns whether anything was stored.
///
/// A logo the detector then found nowhere is not a logo, and caching it would
/// skip relearning on every future recording of this channel, entrenching the
/// mistake — so only a logo that was actually detected is kept.
fn remember_logo(store: Option<&LogoStore>, analysis: &Analysis) -> bool {
    if !analysis.has_logo() {
        return false;
    }
    let (Some(store), Some(logo)) = (store, analysis.learned_logo.as_ref()) else {
        return false;
    };
    match store.save(logo) {
        Ok(_) => true,
        // Failing to cache a logo costs time on the next recording and nothing
        // else, so it must not fail the job.
        Err(error) => {
            tracing::warn!(%error, "could not store the learned logo");
            false
        }
    }
}

/// Log what the scan found, and stop if the recording is beyond saving.
///
/// # Errors
/// Returns [`Error::DamagedSource`] when the recording is mostly scrambled and
/// the caller asked to refuse those.
fn report(diagnostics: Option<&Diagnostics>, refuse_damaged: bool) -> Result<(), Error> {
    let Some(diagnostics) = diagnostics else {
        return Ok(());
    };
    for warning in &diagnostics.warnings {
        tracing::warn!("{warning}");
    }
    if diagnostics.is_hopeless() && refuse_damaged {
        return Err(Error::DamagedSource {
            scrambled: diagnostics.scrambled_packets,
            total: diagnostics.total_packets,
        });
    }
    Ok(())
}

/// Scan the source transport stream for its inventory and health counters.
///
/// Only a transport stream carries any of this: an MP4 has no continuity
/// counters to be discontinuous and nothing to be scrambled. A failure here is
/// logged and otherwise ignored, because a recording that ffmpeg can decode is
/// worth transcoding even if this crate cannot parse its container.
///
/// Public so a caller can record what the source was before starting the work,
/// and hand the result back through [`JobRequest::diagnostics`].
#[must_use]
pub fn inspect(input: &Path) -> Option<Diagnostics> {
    let extension = input
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if !matches!(extension.as_deref(), Some("ts" | "m2ts" | "mts" | "tsv")) {
        return None;
    }

    let file = match std::fs::File::open(input) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, "could not open the source to inspect it");
            return None;
        }
    };
    let size = file.metadata().map_or(0, |m| m.len());

    match asaborake_ts::scan(std::io::BufReader::new(file), size) {
        Ok(info) => Some(Diagnostics::from_ts(&info)),
        Err(error) => {
            tracing::warn!(%error, "could not scan the source transport stream");
            None
        }
    }
}

/// Write the `.cut.json` record beside the output.
fn write_sidecar(
    request: &JobRequest,
    analysis: &Analysis,
    plan: &CutPlan,
    diagnostics: Option<Diagnostics>,
) -> Result<PathBuf, Error> {
    let path = sidecar_path(&request.output);

    // The per-frame logo track is megabytes on a long recording and is only
    // useful to the timeline view, which reads it from the job database.
    let mut trimmed = analysis.clone();
    trimmed.logo_track = None;

    let sidecar = Sidecar {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        input: request.input.clone(),
        profile: request.profile.name.clone(),
        title: request.title.clone(),
        plan: plan.clone(),
        analysis: trimmed,
        diagnostics,
    };

    let json = serde_json::to_string_pretty(&sidecar).map_err(Error::SidecarEncode)?;
    std::fs::write(&path, json).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// The sidecar path for an output file.
#[must_use]
pub fn sidecar_path(output: &Path) -> PathBuf {
    let mut name = output
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    name.push_str(".cut.json");
    output.with_file_name(name)
}

/// A human-readable name for an analysis stage.
fn describe(stage: Stage) -> String {
    match stage {
        Stage::Audio => "measuring audio levels",
        Stage::LocatingLogo => "looking for the station logo",
        Stage::LearningLogo => "learning the station logo",
        Stage::Detecting => "detecting logo and scene changes",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidecar_sits_beside_the_output() {
        assert_eq!(
            sidecar_path(Path::new("/recordings/News.mp4")),
            PathBuf::from("/recordings/News.cut.json")
        );
        assert_eq!(
            sidecar_path(Path::new("/recordings/Drama.ep1.mkv")),
            PathBuf::from("/recordings/Drama.ep1.cut.json")
        );
    }

    #[test]
    fn every_stage_has_a_description() {
        for stage in [
            Stage::Audio,
            Stage::LocatingLogo,
            Stage::LearningLogo,
            Stage::Detecting,
        ] {
            assert!(!describe(stage).is_empty());
        }
    }

    #[test]
    fn only_a_transport_stream_is_scanned() {
        // An MP4 has no continuity counters to be discontinuous and nothing to
        // be scrambled, so reading one through the TS parser would waste a full
        // pass over the file to learn nothing.
        assert!(inspect(Path::new("/recordings/News.mp4")).is_none());
        assert!(inspect(Path::new("/recordings/News")).is_none());
        // A transport stream is scanned — and a missing one fails softly,
        // because ffmpeg is the authority on whether a job can run.
        assert!(inspect(Path::new("/recordings/does-not-exist.ts")).is_none());
    }

    #[test]
    fn a_hopeless_recording_is_refused_only_when_asked() {
        let hopeless = Diagnostics {
            duration_seconds: 1800.0,
            video: None,
            audio: Vec::new(),
            has_captions: false,
            format_changes: Vec::new(),
            split_points: Vec::new(),
            split_offsets: Vec::new(),
            dropped_packets: 0,
            scrambled_packets: 900_000,
            error_packets: 0,
            total_packets: 1_000_000,
            dual_mono: None,
            warnings: Vec::new(),
        };

        // The default is to transcode it anyway: a scan can be wrong about an
        // unusual recording, and a poor file beats no file.
        assert!(report(Some(&hopeless), false).is_ok());

        let error = report(Some(&hopeless), true).expect_err("must refuse");
        assert!(error.to_string().contains("90%"), "{error}");
    }

    #[test]
    fn a_request_defaults_to_learning_logos() {
        let profile = crate::profile::builtin()
            .remove("x264-cpu")
            .expect("profile");
        let request = JobRequest::new("in.ts", "out.mp4", profile);
        assert!(request.learn_logo);
        assert_eq!(request.cut.low_confidence, LowConfidencePolicy::Keep);
    }
}
