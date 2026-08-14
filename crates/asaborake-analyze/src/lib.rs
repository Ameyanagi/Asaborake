//! Frame and audio analysis: everything CM detection reads from.
//!
//! The crate produces one [`Analysis`] per recording, carrying the three weak
//! signals a commercial boundary is found from — where the station logo was
//! present, where the picture cut, and where the audio went quiet — plus the
//! learned logo itself.
//!
//! None of the three is sufficient alone. A logo track has no opinion about
//! *where exactly* a boundary falls, and a channel may drop its logo during
//! the programme itself. Scene changes are everywhere. Silence is everywhere.
//! Their agreement is what carries the information, which is why they are
//! produced together and consumed together by `asaborake-cmcut`.
//!
//! This mirrors how Amatsukaze splits the work between `logoframe` and
//! `chapter_exe` before `join_logo_scp` combines them; see `ATTRIBUTION.md`.

// Tests assert; asserting is how they fail. The workspace bans panicking
// constructs in shipping code, not in the suite that checks it.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod logo;
pub mod scene;

use std::path::{Path, PathBuf};

use asaborake_media::{Ffmpeg, FrameReader, FrameReaderOptions, MediaProbe};
use serde::{Deserialize, Serialize};

pub use logo::model::lgd;
pub use logo::{LogoData, LogoDetector, LogoInterval, LogoLocator, LogoScanner, LogoTrack, Rect};
pub use scene::{SceneChange, SceneDetector, SceneOptions};

/// Level below which audio counts as silent, in dBFS.
///
/// Broadcast inserts a near-digital-silence gap at block boundaries, well
/// below the noise floor of any programme material.
pub const DEFAULT_SILENCE_DBFS: f32 = -50.0;

/// Shortest gap that counts as a silence, in seconds.
///
/// Short enough to catch a tight junction, long enough to ignore the pauses
/// between words.
pub const DEFAULT_SILENCE_SECONDS: f64 = 0.15;

/// Window the loudness envelope is computed over, in seconds.
pub const ENVELOPE_WINDOW_SECONDS: f64 = 0.02;

/// A stretch of quiet audio.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SilentSpan {
    /// Start, in seconds.
    pub start: f64,
    /// End, in seconds.
    pub end: f64,
}

impl SilentSpan {
    /// The midpoint, which is where a boundary is taken to fall.
    #[must_use]
    pub fn centre(&self) -> f64 {
        f64::midpoint(self.start, self.end)
    }

    /// Length in seconds.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// What was learned about a recording's logo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogoSummary {
    /// Where in the frame it sits.
    pub rect: Rect,
    /// Mean opacity across the rectangle.
    pub mean_alpha: f32,
    /// How many flat-background frames the fit used.
    pub frames_used: u32,
    /// Whether it came from the logo store rather than being learned here.
    pub from_store: bool,
}

/// Everything the analysis pass found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// Duration analysed, in seconds.
    pub duration_seconds: f64,
    /// Interval between analysed frames, in seconds.
    pub seconds_per_frame: f64,
    /// The logo, when one was found.
    pub logo: Option<LogoSummary>,
    /// The logo itself, when it was learned by this run.
    ///
    /// Carried in memory so the caller can add it to the logo store, but never
    /// serialised: the coefficient maps are hundreds of kilobytes and belong
    /// in the store's binary form, not in an analysis document.
    #[serde(skip)]
    pub learned_logo: Option<LogoData>,
    /// Spans during which the logo was present.
    pub logo_intervals: Vec<LogoInterval>,
    /// The per-frame logo score, retained for the timeline view.
    pub logo_track: Option<LogoTrack>,
    /// Detected cuts.
    pub scene_changes: Vec<SceneChange>,
    /// Detected silences.
    pub silent_spans: Vec<SilentSpan>,
}

impl Analysis {
    /// Whether a logo was found and looks usable.
    #[must_use]
    pub fn has_logo(&self) -> bool {
        self.logo.is_some() && !self.logo_intervals.is_empty()
    }

    /// Fraction of the recording the logo was present for.
    #[must_use]
    pub fn logo_coverage(&self) -> f64 {
        if self.duration_seconds <= 0.0 {
            return 0.0;
        }
        let covered: f64 = self.logo_intervals.iter().map(LogoInterval::duration).sum();
        (covered / self.duration_seconds).clamp(0.0, 1.0)
    }
}

/// How to analyse a recording.
#[derive(Debug, Clone)]
pub struct AnalysisOptions {
    /// A logo to use instead of learning one.
    ///
    /// Supplying a logo from the store turns a three-pass analysis into a
    /// one-pass one, which is the difference between minutes and seconds on a
    /// long recording. This is the steady state once a channel has been seen.
    pub logo: Option<LogoData>,
    /// Where the logo is, when it is already known.
    ///
    /// Amatsukaze has the operator draw this rectangle, and for a channel
    /// whose logo has been located once by hand it is far more reliable than
    /// finding it again: automatic location has to infer from the picture what
    /// a person can simply see. Supplying it skips the location pass entirely.
    pub logo_rect: Option<Rect>,
    /// Whether to look for a logo at all.
    ///
    /// Off for a channel known to carry none, which skips three decoding
    /// passes and stops the locator settling on a telop banner instead.
    pub find_logo: bool,
    /// Name to give a newly learned logo.
    pub logo_name: String,
    /// Channel a newly learned logo belongs to.
    pub channel_id: Option<String>,
    /// Deinterlace before analysing.
    pub deinterlace: bool,
    /// Decimation for the logo location pass.
    pub locate_step: u32,
    /// Decimation for the logo learning pass.
    pub learn_step: u32,
    /// Level below which audio counts as silent, in dBFS.
    pub silence_dbfs: f32,
    /// Shortest silence worth recording, in seconds.
    pub silence_seconds: f64,
    /// Scene-change tunables.
    pub scene: SceneOptions,
    /// Logo track tunables.
    pub track: logo::TrackOptions,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            logo: None,
            logo_rect: None,
            find_logo: true,
            logo_name: "unknown".to_owned(),
            channel_id: None,
            deinterlace: true,
            // Locating needs only enough frames to average out motion.
            locate_step: 10,
            // Learning needs flat-background frames, which are rare enough
            // that decimating hard would miss them.
            learn_step: 2,
            silence_dbfs: DEFAULT_SILENCE_DBFS,
            silence_seconds: DEFAULT_SILENCE_SECONDS,
            scene: SceneOptions::default(),
            track: logo::TrackOptions::default(),
        }
    }
}

/// Which stage the analysis has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Decoding audio for the loudness envelope.
    Audio,
    /// Looking for where the logo sits.
    LocatingLogo,
    /// Learning the logo's opacity and colour.
    LearningLogo,
    /// Scoring frames and detecting cuts.
    Detecting,
}

/// A progress report from the analysis pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalysisProgress {
    /// The stage in progress.
    pub stage: Stage,
    /// How far through the recording this stage is, in `0.0..=1.0`.
    pub fraction: f64,
}

/// Analyse a recording.
///
/// `on_progress` is called periodically; it must not block, since the frame
/// pipe is not being drained while it runs.
///
/// # Errors
/// Returns [`Error::Media`] when ffmpeg fails or the file has no video.
pub fn analyse(
    ffmpeg: &Ffmpeg,
    input: &Path,
    options: &AnalysisOptions,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<Analysis, Error> {
    let probe = asaborake_media::probe(ffmpeg, input).map_err(Error::Media)?;
    let video = probe.video.as_ref().ok_or_else(|| {
        Error::Media(asaborake_media::Error::NoVideoStream {
            path: input.to_path_buf(),
        })
    })?;
    let duration = probe.duration_seconds.unwrap_or(0.0);

    on_progress(AnalysisProgress {
        stage: Stage::Audio,
        fraction: 0.0,
    });
    let envelope = asaborake_media::rms_envelope(ffmpeg, input, ENVELOPE_WINDOW_SECONDS)
        .map_err(Error::Media)?;
    let silent_spans = envelope
        .silent_spans(options.silence_dbfs, options.silence_seconds)
        .into_iter()
        .map(|(start, end)| SilentSpan { start, end })
        .collect();

    // A logo from the store skips all three learning passes entirely, and a
    // channel known to carry none skips them for good.
    let (logo, from_store) = match options.logo.clone() {
        Some(stored) => (Some(stored), true),
        None if !options.find_logo => (None, false),
        None => (
            learn_logo(ffmpeg, input, &probe, options, duration, on_progress)?,
            false,
        ),
    };
    let learned_logo = if from_store { None } else { logo.clone() };

    let seconds_per_frame = if video.fps() > 0.0 {
        1.0 / video.fps()
    } else {
        0.0
    };

    // Logo scoring and cut detection both need every frame, so they share one
    // pass rather than decoding the recording twice.
    let detected = detect(
        ffmpeg,
        input,
        &probe,
        options,
        logo,
        from_store,
        duration,
        on_progress,
    )?;

    // The container's stated duration is unreliable: a recording made of
    // consecutive broadcast segments reports the length of one of them,
    // because each segment restarts its timebase. What was actually decoded is
    // authoritative, and every cut point is expressed against it.
    let decoded_seconds = detected.frames as f64 * seconds_per_frame;
    let duration_seconds = if decoded_seconds > 0.0 {
        decoded_seconds
    } else {
        duration
    };

    Ok(Analysis {
        duration_seconds,
        seconds_per_frame,
        logo: detected.summary,
        learned_logo,
        logo_intervals: detected.intervals,
        logo_track: detected.track,
        scene_changes: detected.scene_changes,
        silent_spans,
    })
}

/// What the detection pass produced.
struct Detected {
    track: Option<LogoTrack>,
    intervals: Vec<LogoInterval>,
    summary: Option<LogoSummary>,
    scene_changes: Vec<SceneChange>,
    /// Frames actually decoded, which is what the true duration is derived
    /// from.
    frames: u64,
}

/// Score above which a bootstrap detector is taken to have seen the logo.
///
/// Deliberately low. The bootstrap fit is the weaker of the two, so its scores
/// are muted; gating hard on it would reject the very frames the refinement
/// needs.
const REFINEMENT_GATE: f32 = 0.25;

/// Locate and learn a logo.
///
/// # Why three passes
///
/// The first finds where the logo is. The second fits it from every
/// flat-background frame — but a recording's flat frames include the fades
/// inside its *commercials*, where there is no logo at all. Those frames say
/// "observed equals background", which is a different relationship from the
/// one being fitted, and mixing the two drags the estimated opacity toward
/// zero. On a recording that is a third commercials the fit can come out at a
/// third of the true opacity, weak enough to be rejected outright.
///
/// So the third pass uses the second's result to judge which flat frames
/// actually carried the logo, and refits from those alone. Amatsukaze does the
/// same, for the same reason.
///
/// A logo from the store skips all three.
/// What a scan of one rectangle found, whether or not it was usable.
///
/// The failure cases carry their measurements. "No logo could be fitted" on
/// its own tells an operator nothing they can act on: it cannot distinguish a
/// box in the wrong place from a box in the right place over material that
/// could never work, and those need opposite responses.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanReport {
    /// Frames whose surroundings were flat enough to imply a background.
    pub frames_used: u32,
    /// Range of background brightnesses those frames covered, of 255.
    pub background_spread: u8,
    /// Mean opacity across the whole rectangle.
    pub mean_alpha: f32,
    /// Pixels opaque enough to match against.
    pub strong_pixels: usize,
    /// The fit itself, even when it was rejected — seeing it is what tells an
    /// operator whether the box was aimed at the right thing.
    pub logo: Option<LogoData>,
}

impl ScanReport {
    /// What to tell an operator, in a sentence they can act on.
    #[must_use]
    pub fn explain(&self) -> String {
        if self.frames_used < 50 {
            return format!(
                "only {} frames had a flat enough background to use. The box may be \
                 overlapping busy picture — try tightening it around the logo, or pick \
                 a recording with calmer material behind it.",
                self.frames_used
            );
        }
        if self.background_spread < 24 {
            return format!(
                "the background behind the box was always about the same brightness \
                 (a range of {} of 255 across {} frames). Separating a translucent logo \
                 from what is behind it means watching the background change underneath \
                 it, so try a recording with more variety behind the logo.",
                self.background_spread, self.frames_used
            );
        }
        format!(
            "a fit was made from {} frames, but only {} pixels came out opaque enough to \
             match against (mean opacity {:.3}). That usually means the box is not over \
             the logo, or the logo is too faint to separate. Check the preview.",
            self.frames_used, self.strong_pixels, self.mean_alpha
        )
    }
}

/// Scan a rectangle and report what was found, usable or not.
///
/// One pass, and no plausibility bar: this exists to explain a failure, so it
/// must produce the rejected fit rather than hiding it.
///
/// # Errors
/// Returns [`Error::Media`] when the recording cannot be decoded.
pub fn scan_rect(
    ffmpeg: &Ffmpeg,
    input: &Path,
    rect: Rect,
    options: &AnalysisOptions,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<ScanReport, Error> {
    let probe = asaborake_media::probe(ffmpeg, input).map_err(Error::Media)?;
    let duration = probe.duration_seconds.unwrap_or(0.0);
    let Some(video) = probe.video.as_ref() else {
        return Err(Error::Media(asaborake_media::Error::NoVideoStream {
            path: input.to_path_buf(),
        }));
    };

    // A rectangle that reached here was drawn by somebody looking at the
    // picture, so its border is genuine background and is judged end to end.
    let mut scanner = LogoScanner::strict(rect, logo::DEFAULT_FLATNESS_THRESHOLD);
    let mut reader = open_reader(ffmpeg, input, &probe, options, options.learn_step)?;
    while let Some(frame) = reader.next_frame().map_err(Error::Media)? {
        report(&frame, Stage::LearningLogo, duration, on_progress);
        scanner.add_frame(&frame);
    }

    let logo = scanner.finish_bootstrap(
        options.logo_name.clone(),
        options.channel_id.clone(),
        (video.width, video.height),
    );

    Ok(ScanReport {
        frames_used: scanner.frames_accepted(),
        background_spread: scanner.background_spread(),
        mean_alpha: logo.as_ref().map_or(0.0, LogoData::mean_alpha),
        strong_pixels: logo.as_ref().map_or(0, LogoData::strong_pixels),
        logo,
    })
}

/// Learn a logo from one recording, using a rectangle an operator drew.
///
/// This is the logo tool's whole engine side. Automatic location is a fallback
/// for when nobody has aimed yet, and on real broadcast it is the weak link:
/// a permanent telop banner or an emergency overlay looks more like a logo
/// than the logo does. A rectangle drawn by someone who can see the picture
/// removes that guess entirely, which is why Amatsukaze asks for one.
///
/// Returns `None` when no logo could be fitted inside the rectangle — usually
/// because the background behind it never varied enough to separate the logo
/// from it.
///
/// # Errors
/// Returns [`Error::Media`] when the recording cannot be decoded.
pub fn learn(
    ffmpeg: &Ffmpeg,
    input: &Path,
    rect: Rect,
    options: &AnalysisOptions,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<Option<LogoData>, Error> {
    let probe = asaborake_media::probe(ffmpeg, input).map_err(Error::Media)?;
    let duration = probe.duration_seconds.unwrap_or(0.0);
    let options = AnalysisOptions {
        logo_rect: Some(rect),
        ..options.clone()
    };
    learn_logo(ffmpeg, input, &probe, &options, duration, on_progress)
}

fn learn_logo(
    ffmpeg: &Ffmpeg,
    input: &Path,
    probe: &MediaProbe,
    options: &AnalysisOptions,
    duration: f64,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<Option<LogoData>, Error> {
    let Some(video) = probe.video.as_ref() else {
        return Ok(None);
    };
    // A rectangle given by the operator is better evidence than anything the
    // locator can infer from the picture, so it is used as-is and the location
    // pass is skipped. One that does not fit the picture is ignored rather
    // than trusted — a logo file copied from another resolution would
    // otherwise point the scanner at the wrong part of the frame.
    let supplied = options.logo_rect.filter(|rect| {
        let usable = rect.is_valid() && rect.fits_within(video.width, video.height);
        if !usable {
            tracing::warn!(
                ?rect,
                width = video.width,
                height = video.height,
                "the supplied logo rectangle does not fit the picture; locating instead"
            );
        }
        usable
    });

    let rect = if let Some(given) = supplied {
        tracing::info!(?given, "using the logo rectangle that was supplied");
        given
    } else {
        let Some(found) = locate(ffmpeg, input, probe, options, duration, on_progress)? else {
            tracing::info!("no logo region found; falling back to logo-free detection");
            return Ok(None);
        };
        tracing::info!(?found, "located a candidate logo region");
        found
    };

    let Some(bootstrap) = scan_pass(
        ffmpeg,
        input,
        probe,
        options,
        rect,
        None,
        duration,
        on_progress,
    )?
    else {
        tracing::info!("no logo could be fitted from the flat-background frames");
        return Ok(None);
    };
    tracing::debug!(
        alpha = bootstrap.mean_alpha(),
        frames = bootstrap.frames_used,
        "bootstrap logo fitted"
    );

    let Some(mut gate) = LogoDetector::new(bootstrap.clone()) else {
        return Ok(Some(bootstrap));
    };
    let refined = scan_pass(
        ffmpeg,
        input,
        probe,
        options,
        rect,
        Some(&mut gate),
        duration,
        on_progress,
    )?;

    match refined {
        Some(logo) => {
            tracing::info!(
                alpha = logo.mean_alpha(),
                frames = logo.frames_used,
                "refined logo from logo-present frames only"
            );
            Ok(Some(logo))
        }
        // Refinement can reject every frame if the bootstrap was too weak to
        // recognise its own logo. The bootstrap is still the best available —
        // but only if it looks like a logo at all. It was fitted with the
        // plausibility check deliberately switched off, so falling back to it
        // unconditionally is how a fit of nothing at all became a stored
        // "logo": near-zero opacity everywhere, saved against the channel, and
        // reused for every recording from it afterwards.
        None if bootstrap.is_plausible() => {
            tracing::info!(
                alpha = bootstrap.mean_alpha(),
                "refinement found no logo-carrying frames; keeping the bootstrap fit"
            );
            Ok(Some(bootstrap))
        }
        None => {
            tracing::info!(
                alpha = bootstrap.mean_alpha(),
                "nothing in that region looks like a logo"
            );
            Ok(None)
        }
    }
}

/// Find where the logo is, by scanning the recording.
fn locate(
    ffmpeg: &Ffmpeg,
    input: &Path,
    probe: &MediaProbe,
    options: &AnalysisOptions,
    duration: f64,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<Option<Rect>, Error> {
    let Some(video) = probe.video.as_ref() else {
        return Ok(None);
    };

    let mut locator = LogoLocator::new(video.width, video.height);
    let mut reader = open_reader(ffmpeg, input, probe, options, options.locate_step)?;
    while let Some(frame) = reader.next_frame().map_err(Error::Media)? {
        report(&frame, Stage::LocatingLogo, duration, on_progress);
        locator.add_frame(&frame);
    }

    Ok(locator.finish())
}

/// One learning pass, optionally admitting only frames a detector accepts.
#[expect(
    clippy::too_many_arguments,
    reason = "an internal pass over an already-decomposed pipeline"
)]
fn scan_pass(
    ffmpeg: &Ffmpeg,
    input: &Path,
    probe: &MediaProbe,
    options: &AnalysisOptions,
    rect: Rect,
    mut gate: Option<&mut LogoDetector>,
    duration: f64,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<Option<LogoData>, Error> {
    let Some(video) = probe.video.as_ref() else {
        return Ok(None);
    };

    let is_bootstrap = gate.is_none();
    // A rectangle the operator supplied is a tight box round the logo and its
    // border is real background; one the locator inferred is a loose bounding
    // box whose border cannot be held to the same standard.
    let mut scanner = if options.logo_rect.is_some() {
        LogoScanner::strict(rect, logo::DEFAULT_FLATNESS_THRESHOLD)
    } else {
        LogoScanner::new(rect, logo::DEFAULT_FLATNESS_THRESHOLD)
    };
    let mut reader = open_reader(ffmpeg, input, probe, options, options.learn_step)?;

    while let Some(frame) = reader.next_frame().map_err(Error::Media)? {
        report(&frame, Stage::LearningLogo, duration, on_progress);
        if let Some(gate) = gate.as_mut()
            && gate.score(&frame) < REFINEMENT_GATE
        {
            continue;
        }
        scanner.add_frame(&frame);
    }

    let name = options.logo_name.clone();
    let channel = options.channel_id.clone();
    let size = (video.width, video.height);

    // The bootstrap is allowed to be a poor logo — it only has to be good
    // enough to recognise its own frames. The refined fit is held to the bar
    // that decides whether a logo is usable at all.
    Ok(if is_bootstrap {
        scanner.finish_bootstrap(name, channel, size)
    } else {
        scanner.finish(name, channel, size)
    })
}

/// Open a frame reader with the analysis pass's shared settings.
fn open_reader(
    ffmpeg: &Ffmpeg,
    input: &Path,
    probe: &MediaProbe,
    options: &AnalysisOptions,
    step: u32,
) -> Result<FrameReader, Error> {
    FrameReader::open(
        ffmpeg,
        input,
        probe,
        &FrameReaderOptions {
            deinterlace: options.deinterlace,
            select_every: step,
            ..FrameReaderOptions::default()
        },
    )
    .map_err(Error::Media)
}

/// Report progress every so often, rather than on every frame.
fn report(
    frame: &asaborake_media::Frame<'_>,
    stage: Stage,
    duration: f64,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) {
    if frame.index.is_multiple_of(64) {
        on_progress(AnalysisProgress {
            stage,
            fraction: fraction_of(frame.timestamp, duration),
        });
    }
}

/// Score every frame against the logo while detecting cuts in the same pass.
#[expect(
    clippy::too_many_arguments,
    reason = "one internal pass over an already-decomposed pipeline"
)]
fn detect(
    ffmpeg: &Ffmpeg,
    input: &Path,
    probe: &MediaProbe,
    options: &AnalysisOptions,
    logo: Option<LogoData>,
    from_store: bool,
    duration: f64,
    on_progress: &mut dyn FnMut(AnalysisProgress),
) -> Result<Detected, Error> {
    let summary = logo.as_ref().map(|logo| LogoSummary {
        rect: logo.rect,
        mean_alpha: logo.mean_alpha(),
        frames_used: logo.frames_used,
        from_store,
    });
    let mut detector = logo.and_then(LogoDetector::new);

    let mut reader = FrameReader::open(
        ffmpeg,
        input,
        probe,
        &FrameReaderOptions {
            deinterlace: options.deinterlace,
            ..FrameReaderOptions::default()
        },
    )
    .map_err(Error::Media)?;

    let seconds_per_frame = reader.seconds_per_frame();
    let mut scene_detector = SceneDetector::new(seconds_per_frame);
    let mut scores = Vec::new();
    let mut frames = 0u64;

    while let Some(frame) = reader.next_frame().map_err(Error::Media)? {
        if frame.index.is_multiple_of(128) {
            on_progress(AnalysisProgress {
                stage: Stage::Detecting,
                fraction: fraction_of(frame.timestamp, duration),
            });
        }
        frames += 1;
        scene_detector.add_frame(&frame);
        if let Some(detector) = detector.as_mut() {
            scores.push(detector.score(&frame));
        }
    }

    let scene_changes = scene_detector.changes(&options.scene);

    // A recording with no usable logo still yields cuts and silences, which is
    // enough for CM detection to fall back on.
    if scores.is_empty() {
        return Ok(Detected {
            track: None,
            intervals: Vec::new(),
            summary,
            scene_changes,
            frames,
        });
    }

    let track = LogoTrack {
        seconds_per_frame,
        scores,
    };
    let intervals = track.intervals(&options.track);
    Ok(Detected {
        track: Some(track),
        intervals,
        summary,
        scene_changes,
        frames,
    })
}

/// Progress as a fraction, guarding against an unknown duration.
fn fraction_of(position: f64, duration: f64) -> f64 {
    if duration <= 0.0 {
        return 0.0;
    }
    (position / duration).clamp(0.0, 1.0)
}

/// Errors from analysis.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// ffmpeg failed, or the input has no video.
    #[error("media error during analysis")]
    Media(#[source] asaborake_media::Error),

    /// A file could not be read or written.
    #[error("i/o error on {path}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A logo file did not start with the expected magic bytes.
    #[error("not an Asaborake logo file")]
    LogoFormat,

    /// A logo's coefficient arrays did not match its rectangle.
    #[error("logo dimensions are inconsistent")]
    LogoGeometry,

    /// A logo could not be serialised.
    #[error("failed to encode logo")]
    LogoEncode(#[source] postcard::Error),

    /// A logo could not be deserialised.
    #[error("failed to decode logo")]
    LogoDecode(#[source] postcard::Error),

    /// A preview image could not be written.
    #[error("failed to write image {path}")]
    Image {
        /// The path involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: Box<image::ImageError>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_span_geometry() {
        let span = SilentSpan {
            start: 10.0,
            end: 11.0,
        };
        assert!((span.centre() - 10.5).abs() < 1e-9);
        assert!((span.duration() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn logo_coverage_is_the_fraction_of_the_recording() {
        let analysis = Analysis {
            duration_seconds: 100.0,
            seconds_per_frame: 1.0 / 30.0,
            logo: None,
            learned_logo: None,
            logo_intervals: vec![
                LogoInterval {
                    start: 0.0,
                    end: 30.0,
                },
                LogoInterval {
                    start: 60.0,
                    end: 90.0,
                },
            ],
            logo_track: None,
            scene_changes: Vec::new(),
            silent_spans: Vec::new(),
        };
        assert!((analysis.logo_coverage() - 0.6).abs() < 1e-9);
        assert!(!analysis.has_logo(), "no summary means no usable logo");
    }

    #[test]
    fn coverage_of_an_empty_analysis_is_zero() {
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
        assert!((analysis.logo_coverage() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn progress_fraction_is_bounded() {
        assert!((fraction_of(50.0, 100.0) - 0.5).abs() < 1e-9);
        assert!((fraction_of(200.0, 100.0) - 1.0).abs() < 1e-9);
        assert!((fraction_of(5.0, 0.0) - 0.0).abs() < 1e-9);
    }
}
