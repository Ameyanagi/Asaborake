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
    /// Segmentation tunables.
    pub cut: CutOptions,
    /// Whether to learn and store a logo when the channel has none.
    pub learn_logo: bool,
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
            cut: CutOptions::default(),
            learn_logo: true,
        }
    }
}

/// What the pipeline produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobOutcome {
    /// The file that was written.
    pub output: PathBuf,
    /// The sidecar describing the cuts.
    pub sidecar: PathBuf,
    /// What analysis found.
    pub analysis: Analysis,
    /// What was decided.
    pub plan: CutPlan,
    /// Whether a logo was learned and added to the store.
    pub logo_learned: bool,
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
        deinterlace: video.interlaced,
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

    // Keep a freshly learned logo, so the next recording on this channel skips
    // the learning passes entirely.
    let mut logo_learned = false;
    if request.learn_logo
        && let (Some(store), Some(logo)) = (store, analysis.learned_logo.as_ref())
    {
        match store.save(logo) {
            Ok(_) => logo_learned = true,
            // Failing to cache a logo costs time on the next recording and
            // nothing else, so it must not fail the job.
            Err(error) => tracing::warn!(%error, "could not store the learned logo"),
        }
    }

    let plan = asaborake_cmcut::plan(&analysis, &request.cut);
    tracing::info!(
        confidence = plan.confidence,
        cut_seconds = plan.cut_seconds(),
        reason = %plan.reason,
        "cut plan"
    );

    if plan.decision == Decision::KeepAll
        && request.cut.low_confidence == LowConfidencePolicy::Fail
        && plan.confidence < request.cut.confidence_threshold
    {
        return Err(Error::LowConfidence {
            reason: plan.reason.clone(),
        });
    }

    let chapters = crate::chapters::chapters_for(&plan.segments, &plan.keep, plan.decision);

    encode(
        ffmpeg,
        &EncodeRequest {
            input: &request.input,
            output: &request.output,
            profile: &request.profile,
            keep: &plan.keep,
            chapters: &chapters,
            probe: &probe,
        },
        &mut |fraction| {
            on_progress(PipelineProgress {
                fraction: ANALYSIS_SHARE + fraction * (1.0 - ANALYSIS_SHARE),
                message: "encoding".to_owned(),
            });
        },
    )?;

    let sidecar = write_sidecar(request, &analysis, &plan)?;

    on_progress(PipelineProgress {
        fraction: 1.0,
        message: "done".to_owned(),
    });

    Ok(JobOutcome {
        output: request.output.clone(),
        sidecar,
        analysis,
        plan,
        logo_learned,
    })
}

/// Write the `.cut.json` record beside the output.
fn write_sidecar(
    request: &JobRequest,
    analysis: &Analysis,
    plan: &CutPlan,
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
    fn a_request_defaults_to_learning_logos() {
        let profile = crate::profile::builtin()
            .remove("x264-cpu")
            .expect("profile");
        let request = JobRequest::new("in.ts", "out.mp4", profile);
        assert!(request.learn_logo);
        assert_eq!(request.cut.low_confidence, LowConfidencePolicy::Keep);
    }
}
