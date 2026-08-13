//! The ffmpeg process driver for Asaborake.
//!
//! Asaborake never links libav. It spawns `ffmpeg` and `ffprobe` and talks to
//! them over pipes. That choice costs a little throughput and buys three
//! things worth more: the binary works against whatever ffmpeg the host image
//! ships (including the NVENC builds already in the `EPGStation` and mirakc
//! images), the build has no C toolchain dependency, and a decoder that
//! segfaults on a corrupt recording takes down a child process rather than the
//! job server.
//!
//! Amatsukaze reached the same conclusion from the other direction: it drives
//! a modified ffmpeg and a chain of external executables rather than embedding
//! them. See `ATTRIBUTION.md`.

// Tests assert; asserting is how they fail. The workspace bans panicking
// constructs in shipping code, not in the suite that checks it.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod audio;
pub mod encode;
pub mod ffmpeg;
pub mod frames;
pub mod probe;
pub mod run;
pub mod still;

pub use audio::{RmsEnvelope, rms_envelope};
pub use encode::{Chapter, ffmetadata, progress_args};
pub use ffmpeg::{Ffmpeg, MINIMUM_FFMPEG_VERSION};
pub use frames::{Frame, FrameReader, FrameReaderOptions};
pub use probe::{AudioStream, MediaProbe, VideoStream, probe};
pub use run::{Progress, run_with_progress};
pub use still::{MAX_WIDTH, still_png};

use std::path::PathBuf;

/// Errors from driving ffmpeg.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An ffmpeg or ffprobe binary could not be executed.
    #[error("cannot run {path}; is ffmpeg installed and on PATH?")]
    FfmpegMissing {
        /// The path that could not be executed.
        path: PathBuf,
        /// The underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// The binary ran but its version banner could not be understood.
    #[error("cannot determine the version of {path}")]
    FfmpegUnreadableVersion {
        /// The binary whose banner was unreadable.
        path: PathBuf,
    },

    /// The installed ffmpeg predates a feature Asaborake depends on.
    #[error(
        "ffmpeg {}.{} is too old; {}.{} or newer is required for -fps_mode",
        found.0, found.1, required.0, required.1
    )]
    FfmpegTooOld {
        /// Version that was found.
        found: (u32, u32),
        /// Minimum version Asaborake accepts.
        required: (u32, u32),
    },

    /// A child process could not be started.
    #[error("failed to start {program}")]
    Spawn {
        /// The program that could not be started.
        program: String,
        /// The underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// A child process exited with a non-zero status.
    #[error("{program} exited with {}\n{stderr}", code.map_or_else(|| "a signal".to_owned(), |c| format!("status {c}")))]
    Failed {
        /// The program that failed.
        program: String,
        /// Exit status, absent when the process was killed by a signal.
        code: Option<i32>,
        /// The tail of the process's stderr.
        stderr: String,
    },

    /// A pipe to or from a child process failed.
    #[error("i/o error while talking to ffmpeg")]
    Io {
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// ffprobe's JSON output could not be parsed.
    #[error("cannot interpret ffprobe output for {path}")]
    ProbeParse {
        /// The file that was probed.
        path: PathBuf,
        /// The underlying deserialisation failure.
        #[source]
        source: serde_json::Error,
    },

    /// The file has no video stream to analyse.
    #[error("{path} has no video stream")]
    NoVideoStream {
        /// The file that was opened.
        path: PathBuf,
    },
}
