//! Probing a file with `ffprobe`.
//!
//! `asaborake-ts` is the authority on what a recording contains; this exists
//! because the analysis and encode passes need to agree with ffmpeg about
//! *ffmpeg's* view of the file — which stream index it will select, what pixel
//! format it will hand back, what frame rate it will impose. Disagreeing with
//! the decoder about any of those misplaces every cut point.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::ffmpeg::Ffmpeg;
use crate::run::capture_stdout;

/// The video stream ffmpeg will select.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoStream {
    /// Stream index within the file.
    pub index: u32,
    /// Codec short name, e.g. `mpeg2video`.
    pub codec: String,
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Frame rate as an exact rational.
    pub frame_rate: (u32, u32),
    /// Pixel format, e.g. `yuv420p`.
    pub pixel_format: String,
    /// Whether ffmpeg reports the content as interlaced.
    pub interlaced: bool,
}

impl VideoStream {
    /// Frame rate as a floating-point value.
    #[must_use]
    pub fn fps(&self) -> f64 {
        if self.frame_rate.1 == 0 {
            return 0.0;
        }
        f64::from(self.frame_rate.0) / f64::from(self.frame_rate.1)
    }
}

/// One audio stream ffmpeg reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioStream {
    /// Stream index within the file.
    pub index: u32,
    /// Codec short name, e.g. `aac`.
    pub codec: String,
    /// Channel count as coded.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

/// What ffmpeg reports about a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaProbe {
    /// Container duration in seconds, when the container states one.
    pub duration_seconds: Option<f64>,
    /// The first video stream, if any.
    pub video: Option<VideoStream>,
    /// Every audio stream, in file order.
    pub audio: Vec<AudioStream>,
}

impl MediaProbe {
    /// Whether the programme is dual-mono, which Japanese broadcast uses for
    /// bilingual audio and which must be downmixed deliberately rather than
    /// merged into a stereo pair.
    #[must_use]
    pub fn is_dual_mono(&self) -> bool {
        self.audio.len() >= 2 && self.audio.iter().all(|a| a.channels == 1)
    }
}

/// Run `ffprobe` against a file and interpret its JSON output.
///
/// # Errors
/// Returns [`Error::Failed`] when ffprobe rejects the file, or
/// [`Error::ProbeParse`] when its output cannot be understood.
pub fn probe(ffmpeg: &Ffmpeg, input: &Path) -> Result<MediaProbe, Error> {
    let mut command = std::process::Command::new(ffmpeg.ffprobe_path());
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]);
    command.arg(input);

    let output = capture_stdout(command)?;
    let raw: RawProbe = serde_json::from_slice(&output).map_err(|source| Error::ProbeParse {
        path: input.to_path_buf(),
        source,
    })?;

    let duration_seconds = raw
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| d.is_finite() && *d > 0.0);

    let video = raw
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .and_then(|s| {
            Some(VideoStream {
                index: s.index,
                codec: s.codec_name.clone().unwrap_or_else(|| "unknown".into()),
                width: s.width?,
                height: s.height?,
                // `avg_frame_rate` is averaged over the whole file and reads
                // 0/0 on a stream ffmpeg could not time; `r_frame_rate` is the
                // container's nominal rate and is the better fallback.
                frame_rate: parse_rational(s.avg_frame_rate.as_deref())
                    .or_else(|| parse_rational(s.r_frame_rate.as_deref()))
                    .unwrap_or((30000, 1001)),
                pixel_format: s.pix_fmt.clone().unwrap_or_else(|| "yuv420p".into()),
                interlaced: s
                    .field_order
                    .as_deref()
                    .is_some_and(|order| order != "progressive"),
            })
        });

    let audio = raw
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .map(|s| AudioStream {
            index: s.index,
            codec: s.codec_name.clone().unwrap_or_else(|| "unknown".into()),
            channels: s.channels.unwrap_or(2),
            sample_rate: s
                .sample_rate
                .as_deref()
                .and_then(|r| r.parse().ok())
                .unwrap_or(48_000),
        })
        .collect();

    Ok(MediaProbe {
        duration_seconds,
        video,
        audio,
    })
}

/// Parse an ffprobe rational such as `30000/1001`.
///
/// ffprobe writes `0/0` for a rate it could not determine, which must not be
/// mistaken for a valid zero frame rate.
fn parse_rational(value: Option<&str>) -> Option<(u32, u32)> {
    let (numerator, denominator) = value?.split_once('/')?;
    let numerator: u32 = numerator.parse().ok()?;
    let denominator: u32 = denominator.parse().ok()?;
    if numerator == 0 || denominator == 0 {
        return None;
    }
    Some((numerator, denominator))
}

/// The subset of ffprobe's JSON schema Asaborake reads.
#[derive(Debug, Deserialize)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    pix_fmt: Option<String>,
    field_order: Option<String>,
    channels: Option<u32>,
    sample_rate: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_rationals_and_rejects_the_unknown_marker() {
        assert_eq!(parse_rational(Some("30000/1001")), Some((30000, 1001)));
        assert_eq!(parse_rational(Some("25/1")), Some((25, 1)));
        assert_eq!(parse_rational(Some("0/0")), None);
        assert_eq!(parse_rational(Some("30")), None);
        assert_eq!(parse_rational(None), None);
    }

    #[test]
    fn dual_mono_is_two_single_channel_streams() {
        let mono = |index| AudioStream {
            index,
            codec: "aac".into(),
            channels: 1,
            sample_rate: 48_000,
        };
        let stereo = AudioStream {
            index: 1,
            codec: "aac".into(),
            channels: 2,
            sample_rate: 48_000,
        };

        let dual = MediaProbe {
            duration_seconds: Some(60.0),
            video: None,
            audio: vec![mono(1), mono(2)],
        };
        assert!(dual.is_dual_mono());

        let single = MediaProbe {
            duration_seconds: Some(60.0),
            video: None,
            audio: vec![stereo],
        };
        assert!(!single.is_dual_mono());
    }

    #[test]
    fn interprets_an_ffprobe_document() {
        let json = br#"{
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "mpeg2video",
                 "width": 1440, "height": 1080, "avg_frame_rate": "30000/1001",
                 "r_frame_rate": "30000/1001", "pix_fmt": "yuv420p",
                 "field_order": "tt"},
                {"index": 1, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": "48000"}
            ],
            "format": {"duration": "1800.5"}
        }"#;
        let raw: RawProbe = serde_json::from_slice(json).expect("valid probe json");
        assert_eq!(raw.streams.len(), 2);
        assert_eq!(raw.streams[0].width, Some(1440));
        assert_eq!(raw.streams[0].field_order.as_deref(), Some("tt"));
        assert_eq!(
            raw.format.and_then(|f| f.duration).as_deref(),
            Some("1800.5")
        );
    }

    #[test]
    fn falls_back_to_the_nominal_rate_when_the_average_is_unknown() {
        // A recording ffmpeg could not time reports 0/0 as the average.
        assert_eq!(
            parse_rational(Some("0/0")).or_else(|| parse_rational(Some("30000/1001"))),
            Some((30000, 1001))
        );
    }
}
