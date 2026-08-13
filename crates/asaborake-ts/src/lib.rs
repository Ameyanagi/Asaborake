//! MPEG-2 transport stream demultiplexing for Asaborake.
//!
//! This crate reads a Japanese broadcast recording and answers the questions
//! the rest of the pipeline needs before it can touch a single frame: which
//! PIDs carry the programme, how long it actually runs, whether the picture
//! geometry changes part-way through, and whether the recording is healthy
//! enough to be worth encoding at all.
//!
//! It deliberately does no decoding. Pixels come from ffmpeg, via
//! `asaborake-media`; this crate only reads the container.
//!
//! The approach follows Amatsukaze's `Mpeg2TsParser.hpp` and `TsInfo.hpp` in
//! spirit — see `ATTRIBUTION.md`.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let file = std::fs::File::open("recording.ts")?;
//! let size = file.metadata()?.len();
//! let info = asaborake_ts::scan(std::io::BufReader::new(file), size)?;
//! println!("{:.1}s, {} programs", info.duration_seconds, info.programs.len());
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod packet;
pub mod pes;
pub mod psi;
pub mod scan;
pub mod video;

pub use packet::{PacketLayout, TsPacket};
pub use pes::{PesHeader, PtsUnwrapper};
pub use psi::{EsInfo, Pat, Pmt, StreamKind};
pub use scan::{FormatChange, ProgramInfo, StreamInfo, TsInfo, TsStats, scan};
pub use video::VideoFormat;

/// Errors produced while reading a transport stream.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input never presented a repeating sync byte at any known stride,
    /// so it is not a transport stream in any layout this crate reads.
    #[error("no transport stream sync pattern found; input is not an MPEG-2 TS")]
    NoSync,

    /// The underlying reader failed.
    #[error("failed to read transport stream")]
    Io(#[source] std::io::Error),
}
