//! What the recording was, and what was wrong with it.
//!
//! A transcode can succeed and still be a bad outcome: a third of the packets
//! scrambled because the card failed, a thousand drops because the aerial is
//! marginal, a second language quietly absent, the picture geometry changing
//! half way through. None of that stops ffmpeg, and none of it is visible in
//! the output file.
//!
//! Amatsukaze reports all of it — error counters, audio drift statistics, the
//! stream inventory — in its per-job history, which is how an operator notices
//! that a channel has been recording badly for a fortnight. This is the same
//! idea: measure it once, during the pass that was already reading the file,
//! and keep it with the job.

use asaborake_ts::{StreamKind, TsInfo};
use serde::{Deserialize, Serialize};

/// A bilingual programme's two languages, main channel first.
///
/// Japanese broadcast carries 二か国語 as a single audio stream in ARIB's
/// "1/0 + 1/0 mode": two independent mono programmes, one per channel. A
/// decoder sees an ordinary stereo pair, so treating it as one means both
/// languages come out of the speakers at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DualMono {
    /// Language on the left channel, which carries the main programme.
    pub main: Option<String>,
    /// Language on the right channel.
    pub sub: Option<String>,
}

/// How healthy a recording was, and what it contained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Duration as measured from the transport stream, in seconds.
    pub duration_seconds: f64,
    /// Picture geometry at the start of the recording.
    pub video: Option<String>,
    /// One entry per audio stream, describing what it carries.
    pub audio: Vec<String>,
    /// Whether the recording carries ARIB captions.
    pub has_captions: bool,
    /// Points at which the video format changed at all, in seconds.
    pub format_changes: Vec<f64>,
    /// Points at which the picture *geometry* changed, in seconds.
    ///
    /// A subset of [`format_changes`](Self::format_changes): a frame-rate
    /// change is re-timed by the encoder and needs nothing done about it, but
    /// a size change cannot go in the same output file, because a video track
    /// has one size for its whole length.
    #[serde(default)]
    pub split_points: Vec<f64>,
    /// Byte offsets in the source file of those same points.
    ///
    /// Splitting a transport stream by byte is the only reliable way to
    /// separate two picture sizes: both the filter clock and the timestamps
    /// break at the change, and packet boundaries do not.
    #[serde(default)]
    pub split_offsets: Vec<u64>,
    /// Packets lost, inferred from continuity counters.
    pub dropped_packets: u64,
    /// Packets still scrambled, meaning decryption did not happen.
    pub scrambled_packets: u64,
    /// Packets the demodulator flagged as uncorrectable.
    pub error_packets: u64,
    /// Total packets read, for putting the counters in proportion.
    pub total_packets: u64,
    /// The two languages a bilingual programme carries, main first.
    ///
    /// Set only when the recording declares itself dual mono, which is the one
    /// case where the two channels of an audio stream are not left and right
    /// but two separate programmes.
    #[serde(default)]
    pub dual_mono: Option<DualMono>,
    /// Anything an operator should be told about, in plain words.
    pub warnings: Vec<String>,
}

impl Diagnostics {
    /// Summarise a transport stream scan.
    #[must_use]
    pub fn from_ts(info: &TsInfo) -> Self {
        let program = info.primary_program();

        let video = info.video_format.map(|format| {
            format!(
                "{}x{} {:.3} fps{}",
                format.width,
                format.height,
                format.fps(),
                if format.interlaced { " interlaced" } else { "" }
            )
        });

        let audio = program
            .map(|program| {
                program
                    .audio()
                    .iter()
                    .map(|stream| format!("{} on pid {:#06x}", name_of(stream.kind), stream.pid))
                    .collect()
            })
            .unwrap_or_default();

        let has_captions = program.is_some_and(|program| {
            program
                .streams
                .iter()
                .any(|stream| stream.kind == StreamKind::Caption)
        });

        // Dual mono lives on the main audio stream; a programme does not carry
        // a second bilingual stream alongside it.
        let dual_mono = program
            .and_then(|program| {
                program
                    .audio()
                    .into_iter()
                    .find(|stream| stream.is_dual_mono())
            })
            .and_then(|stream| stream.audio.as_ref())
            .map(|component| DualMono {
                main: component.language.clone(),
                sub: component.second_language.clone(),
            });

        let mut diagnostics = Self {
            duration_seconds: info.duration_seconds,
            video,
            audio,
            has_captions,
            format_changes: info.format_changes.iter().map(|c| c.seconds).collect(),
            split_points: splits(info).iter().map(|c| c.seconds).collect(),
            split_offsets: splits(info).iter().map(|c| c.byte_offset).collect(),
            dropped_packets: info.stats.dropped_packets,
            scrambled_packets: info.stats.scrambled_packets,
            error_packets: info.stats.error_packets,
            total_packets: info.packet_count,
            dual_mono,
            warnings: Vec::new(),
        };
        diagnostics.warnings = diagnostics.describe_problems();
        diagnostics
    }

    /// Turn the counters into sentences worth reading.
    ///
    /// Thresholds rather than raw numbers, because "412 dropped packets" means
    /// nothing without knowing there were nine million of them.
    fn describe_problems(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let total = self.total_packets.max(1) as f64;

        let scrambled = self.scrambled_packets as f64 / total;
        if scrambled > 0.30 {
            warnings.push(format!(
                "{:.0}% of the recording is still scrambled — decryption failed, and the \
                 result will be unwatchable",
                scrambled * 100.0
            ));
        } else if scrambled > 0.001 {
            warnings.push(format!(
                "{:.1}% of packets are scrambled; some of the recording will not decode",
                scrambled * 100.0
            ));
        }

        let dropped = self.dropped_packets as f64 / total;
        if dropped > 0.01 {
            warnings.push(format!(
                "{:.1}% of packets were lost — reception was poor, expect glitches",
                dropped * 100.0
            ));
        }

        if self.error_packets > 0 {
            warnings.push(format!(
                "{} packets arrived corrupt and were discarded",
                self.error_packets
            ));
        }

        if !self.split_points.is_empty() {
            warnings.push(format!(
                "the picture size changes {} time(s) mid-recording, so the output is \
                 written as {} separate files",
                self.split_points.len(),
                self.split_points.len() + 1
            ));
        }

        if self.has_captions {
            // Stated plainly because it is a known, deliberate gap rather than
            // a fault in this recording.
            warnings.push(
                "this recording carries captions, which Asaborake does not yet extract".to_owned(),
            );
        }

        if self.audio.len() > 1 {
            warnings.push(format!(
                "{} audio tracks, all carried through to the output",
                self.audio.len()
            ));
        }

        if let Some(dual) = &self.dual_mono {
            warnings.push(format!(
                "bilingual audio ({} and {}); the two channels are split into \
                 separate tracks",
                dual.main.as_deref().unwrap_or("unknown"),
                dual.sub.as_deref().unwrap_or("unknown"),
            ));
        }

        warnings
    }

    /// Whether anything here should stop the job rather than merely be noted.
    ///
    /// A mostly-scrambled recording cannot be improved by transcoding it, and
    /// spending an hour of GPU time to produce an unwatchable file helps
    /// nobody.
    #[must_use]
    pub fn is_hopeless(&self) -> bool {
        let total = self.total_packets.max(1) as f64;
        self.scrambled_packets as f64 / total > 0.30
    }
}

/// Where the picture size changes, compared against what preceded it.
///
/// Against the *previous* format rather than the first, because a recording
/// that goes HD, SD, HD needs a boundary at both changes — comparing
/// everything to the opening format would miss the second one.
fn splits(info: &TsInfo) -> Vec<asaborake_ts::FormatChange> {
    let mut points = Vec::new();
    let mut previous = info.video_format;
    for change in &info.format_changes {
        if previous.is_some_and(|before| before.requires_split(&change.format)) {
            points.push(*change);
        }
        previous = Some(change.format);
    }
    points
}

/// A name for a stream kind that reads as English rather than as Rust.
///
/// The debug spelling of the enum leaks variant names into an operator's
/// screen, which is a small thing that makes a tool feel unfinished.
fn name_of(kind: StreamKind) -> String {
    match kind {
        StreamKind::Mpeg2Video => "MPEG-2 video".to_owned(),
        StreamKind::H264Video => "H.264 video".to_owned(),
        StreamKind::HevcVideo => "HEVC video".to_owned(),
        StreamKind::AacAudio => "AAC audio".to_owned(),
        StreamKind::Caption => "captions".to_owned(),
        StreamKind::Superimpose => "superimpose".to_owned(),
        StreamKind::Data => "data".to_owned(),
        StreamKind::Other(stream_type) => format!("stream type {stream_type:#04x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asaborake_ts::{PacketLayout, ProgramInfo, StreamInfo, TsStats, VideoFormat};

    fn info(stats: TsStats, packets: u64, streams: Vec<StreamInfo>) -> TsInfo {
        TsInfo {
            layout: PacketLayout::Ts188,
            packet_count: packets,
            file_size: packets * 188,
            duration_seconds: 1800.0,
            programs: vec![ProgramInfo {
                program_number: 1024,
                pmt_pid: 0x0100,
                pcr_pid: 0x0111,
                streams,
            }],
            video_format: Some(VideoFormat {
                width: 1440,
                height: 1080,
                frame_rate: (30000, 1001),
                interlaced: true,
            }),
            format_changes: Vec::new(),
            stats,
        }
    }

    fn stream(pid: u16, kind: StreamKind, stream_type: u8) -> StreamInfo {
        StreamInfo {
            pid,
            stream_type,
            kind,
            component_tag: None,
            audio: None,
        }
    }

    /// An audio stream carrying two languages, one per channel.
    fn bilingual(pid: u16) -> StreamInfo {
        StreamInfo {
            audio: Some(asaborake_ts::AudioComponent {
                component_type: 0x02,
                component_tag: 0x10,
                language: Some("jpn".to_owned()),
                second_language: Some("eng".to_owned()),
            }),
            ..stream(pid, StreamKind::AacAudio, 0x0F)
        }
    }

    fn healthy() -> Vec<StreamInfo> {
        vec![
            stream(0x0111, StreamKind::Mpeg2Video, 0x02),
            stream(0x0112, StreamKind::AacAudio, 0x0F),
        ]
    }

    #[test]
    fn a_clean_recording_has_nothing_to_report() {
        let diagnostics = Diagnostics::from_ts(&info(TsStats::default(), 1_000_000, healthy()));
        assert!(
            diagnostics.warnings.is_empty(),
            "{:?}",
            diagnostics.warnings
        );
        assert!(!diagnostics.is_hopeless());
        assert_eq!(diagnostics.audio.len(), 1);
        assert!(!diagnostics.has_captions);
    }

    #[test]
    fn a_mostly_scrambled_recording_is_called_hopeless() {
        // The card or SoftCAS failed. Transcoding it cannot help, and an hour
        // of GPU time would produce an unwatchable file.
        let stats = TsStats {
            scrambled_packets: 600_000,
            ..TsStats::default()
        };
        let diagnostics = Diagnostics::from_ts(&info(stats, 1_000_000, healthy()));

        assert!(diagnostics.is_hopeless());
        assert!(
            diagnostics
                .warnings
                .iter()
                .any(|w| w.contains("unwatchable")),
            "{:?}",
            diagnostics.warnings
        );
    }

    #[test]
    fn a_trace_of_scrambling_is_noted_but_not_fatal() {
        let stats = TsStats {
            scrambled_packets: 5_000,
            ..TsStats::default()
        };
        let diagnostics = Diagnostics::from_ts(&info(stats, 1_000_000, healthy()));

        assert!(!diagnostics.is_hopeless());
        assert!(
            diagnostics.warnings.iter().any(|w| w.contains("scrambled")),
            "{:?}",
            diagnostics.warnings
        );
    }

    #[test]
    fn poor_reception_is_described_in_proportion() {
        // A raw count means nothing without knowing the total.
        let stats = TsStats {
            dropped_packets: 50_000,
            ..TsStats::default()
        };
        let diagnostics = Diagnostics::from_ts(&info(stats, 1_000_000, healthy()));
        assert!(
            diagnostics.warnings.iter().any(|w| w.contains("reception")),
            "{:?}",
            diagnostics.warnings
        );

        // The same absolute count against a much longer recording is normal.
        let quiet = Diagnostics::from_ts(&info(stats, 100_000_000, healthy()));
        assert!(
            !quiet.warnings.iter().any(|w| w.contains("reception")),
            "{:?}",
            quiet.warnings
        );
    }

    #[test]
    fn captions_are_reported_as_a_known_gap() {
        let mut streams = healthy();
        streams.push(stream(0x0114, StreamKind::Caption, 0x06));
        let diagnostics = Diagnostics::from_ts(&info(TsStats::default(), 1_000_000, streams));

        assert!(diagnostics.has_captions);
        assert!(
            diagnostics.warnings.iter().any(|w| w.contains("captions")),
            "{:?}",
            diagnostics.warnings
        );
    }

    #[test]
    fn a_second_audio_track_is_reported() {
        let mut streams = healthy();
        streams.push(stream(0x0113, StreamKind::AacAudio, 0x0F));
        let diagnostics = Diagnostics::from_ts(&info(TsStats::default(), 1_000_000, streams));

        assert_eq!(diagnostics.audio.len(), 2);
        assert!(
            diagnostics
                .warnings
                .iter()
                .any(|w| w.contains("audio tracks")),
            "{:?}",
            diagnostics.warnings
        );
    }

    #[test]
    fn a_bilingual_stream_is_recognised_from_the_pmt() {
        // One stream, two languages, one per channel. Nothing about the stream
        // itself distinguishes it from stereo — only this descriptor does.
        let streams = vec![
            stream(0x0111, StreamKind::Mpeg2Video, 0x02),
            bilingual(0x0112),
        ];
        let diagnostics = Diagnostics::from_ts(&info(TsStats::default(), 1_000_000, streams));

        let dual = diagnostics.dual_mono.as_ref().expect("bilingual");
        assert_eq!(dual.main.as_deref(), Some("jpn"));
        assert_eq!(dual.sub.as_deref(), Some("eng"));
        assert!(
            diagnostics.warnings.iter().any(|w| w.contains("bilingual")),
            "{:?}",
            diagnostics.warnings
        );

        // One audio *stream*, so the multiple-tracks note must not also fire.
        assert_eq!(diagnostics.audio.len(), 1);
        assert!(
            !diagnostics
                .warnings
                .iter()
                .any(|w| w.contains("audio tracks")),
            "{:?}",
            diagnostics.warnings
        );
    }

    #[test]
    fn an_ordinary_recording_is_not_called_bilingual() {
        let diagnostics = Diagnostics::from_ts(&info(TsStats::default(), 1_000_000, healthy()));
        assert_eq!(diagnostics.dual_mono, None);
    }

    #[test]
    fn the_inventory_reads_as_english() {
        // The debug spelling of the enum would put "AacAudio" on an operator's
        // screen, which is the kind of leak that makes a tool feel unfinished.
        let diagnostics = Diagnostics::from_ts(&info(TsStats::default(), 1_000_000, healthy()));
        assert_eq!(diagnostics.audio, vec!["AAC audio on pid 0x0112"]);
        assert_eq!(
            diagnostics.video.as_deref(),
            Some("1440x1080 29.970 fps interlaced")
        );
    }
}
