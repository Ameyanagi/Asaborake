//! Profiles, the logo store, and the job pipeline.
//!
//! This crate is where the analysis and segmentation crates are turned into
//! something that produces a file. It owns the decisions an operator can
//! reasonably change — which encoder, what quality, what container — and fixes
//! the ones they cannot, because getting those wrong yields broken output
//! rather than differently-shaped output.

// Tests assert; asserting is how they fail. The workspace bans panicking
// constructs in shipping code, not in the suite that checks it.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

pub mod chapters;
pub mod diagnostics;
pub mod encode;
pub mod pipeline;
pub mod profile;
pub mod store;

use std::path::PathBuf;

pub use diagnostics::Diagnostics;
pub use encode::{EncodeRequest, encode};
pub use pipeline::{JobOutcome, JobRequest, PipelineProgress, Sidecar, run};
pub use profile::{Container, Profile, builtin};
pub use store::LogoStore;

/// Errors from the pipeline.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// ffmpeg failed.
    #[error("media error")]
    Media(#[source] asaborake_media::Error),

    /// Analysis failed.
    #[error("analysis error")]
    Analyze(#[source] asaborake_analyze::Error),

    /// The ffmpeg build cannot run the requested profile.
    #[error("profile '{profile}' needs the {encoder} encoder, which this ffmpeg does not have")]
    UnsupportedProfile {
        /// The profile that was asked for.
        profile: String,
        /// The encoder it needs.
        encoder: String,
    },

    /// Commercial detection was not confident and the policy is to fail.
    #[error("commercial detection was not confident enough: {reason}")]
    LowConfidence {
        /// What the segmenter reported.
        reason: String,
    },

    /// The source is too damaged to be worth transcoding.
    #[error(
        "the recording is {}% scrambled, so decryption failed and transcoding it \
         would only produce an unwatchable file",
        scrambled.saturating_mul(100) / (*total).max(1)
    )]
    DamagedSource {
        /// Packets that were still scrambled.
        scrambled: u64,
        /// Packets read in total.
        total: u64,
    },

    /// A profile document could not be parsed.
    #[error("cannot parse profile")]
    ProfileParse {
        /// The underlying failure.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// A profile could not be serialised.
    #[error("cannot serialise profile")]
    ProfileEncode {
        /// The underlying failure.
        #[source]
        source: Box<toml::ser::Error>,
    },

    /// The cut sidecar could not be serialised.
    #[error("cannot serialise the cut record")]
    SidecarEncode(#[source] serde_json::Error),

    /// A file could not be read or written.
    #[error("i/o error on {path}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}
