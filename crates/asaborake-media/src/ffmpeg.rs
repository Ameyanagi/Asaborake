//! Locating and describing the ffmpeg installation Asaborake drives.
//!
//! Asaborake never links libav. It spawns `ffmpeg` and `ffprobe` and talks to
//! them over pipes, which means it works against whatever build the host image
//! already ships — including the NVENC-enabled builds in the EPGStation and
//! mirakc images — without a compile-time dependency on any of it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Error;

/// The oldest ffmpeg Asaborake supports.
///
/// 5.1 is the release that introduced `-fps_mode`, which the frame readers
/// rely on to keep the frame-index-to-timestamp mapping exact. Older builds
/// would need the deprecated `-vsync`, and silently producing a variable frame
/// rate would misplace every cut point.
pub const MINIMUM_FFMPEG_VERSION: (u32, u32) = (5, 1);

/// A located ffmpeg installation.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    version: (u32, u32),
    encoders: Vec<String>,
}

impl Ffmpeg {
    /// Locate ffmpeg and ffprobe, defaulting to whatever is on `PATH`.
    ///
    /// # Errors
    /// Returns [`Error::FfmpegMissing`] when a binary cannot be executed, or
    /// [`Error::FfmpegTooOld`] when the build predates [`MINIMUM_FFMPEG_VERSION`].
    pub fn discover(ffmpeg: Option<&Path>, ffprobe: Option<&Path>) -> Result<Self, Error> {
        let ffmpeg = ffmpeg.map_or_else(|| PathBuf::from("ffmpeg"), Path::to_path_buf);
        let ffprobe = ffprobe.map_or_else(|| PathBuf::from("ffprobe"), Path::to_path_buf);

        let banner = Command::new(&ffmpeg)
            .arg("-version")
            .output()
            .map_err(|source| Error::FfmpegMissing {
                path: ffmpeg.clone(),
                source,
            })?;
        let banner = String::from_utf8_lossy(&banner.stdout);
        let version = parse_version(&banner).ok_or_else(|| Error::FfmpegUnreadableVersion {
            path: ffmpeg.clone(),
        })?;
        if version < MINIMUM_FFMPEG_VERSION {
            return Err(Error::FfmpegTooOld {
                found: version,
                required: MINIMUM_FFMPEG_VERSION,
            });
        }

        // Probing ffprobe separately catches the common misconfiguration where
        // only one of the pair is installed.
        Command::new(&ffprobe)
            .arg("-version")
            .output()
            .map_err(|source| Error::FfmpegMissing {
                path: ffprobe.clone(),
                source,
            })?;

        let encoders = list_encoders(&ffmpeg);

        Ok(Self {
            ffmpeg,
            ffprobe,
            version,
            encoders,
        })
    }

    /// Path to the `ffmpeg` binary.
    #[must_use]
    pub fn ffmpeg_path(&self) -> &Path {
        &self.ffmpeg
    }

    /// Path to the `ffprobe` binary.
    #[must_use]
    pub fn ffprobe_path(&self) -> &Path {
        &self.ffprobe
    }

    /// Major and minor version of the ffmpeg build.
    #[must_use]
    pub const fn version(&self) -> (u32, u32) {
        self.version
    }

    /// Whether the build offers the named encoder, e.g. `h264_nvenc`.
    ///
    /// Profiles are checked against this before a job starts, so a missing
    /// NVENC build fails immediately with a clear message rather than after
    /// the analysis pass has already run.
    #[must_use]
    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.iter().any(|e| e == name)
    }

    /// Every encoder this build exposes, sorted.
    #[must_use]
    pub fn encoders(&self) -> &[String] {
        &self.encoders
    }

    /// Start an ffmpeg command with the flags every Asaborake invocation wants:
    /// no banner, no stdin, and errors only on stderr.
    #[must_use]
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.ffmpeg);
        command.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
        command
    }
}

/// Pull `major.minor` out of the `ffmpeg -version` banner.
///
/// Distribution builds report things like `n6.1.1` or `6.0-static` or a bare
/// git hash; the last of those has no version to find and returns `None`.
fn parse_version(banner: &str) -> Option<(u32, u32)> {
    let rest = banner.split("ffmpeg version ").nth(1)?;
    let token = rest.split_whitespace().next()?;
    let token = token.trim_start_matches('n');
    let mut parts = token.split(['.', '-', '_']);
    let major = parts.next()?.parse().ok()?;
    // A build reporting only a major version is treated as `.0`.
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// Ask ffmpeg which encoders it was built with.
fn list_encoders(ffmpeg: &Path) -> Vec<String> {
    let Ok(output) = Command::new(ffmpeg).args(["-hide_banner", "-encoders"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut encoders: Vec<String> = text
        .lines()
        // Entries are indented and prefixed by a six-character capability
        // field, e.g. " V....D h264_nvenc  NVIDIA NVENC H.264 encoder".
        .filter_map(|line| {
            let line = line.strip_prefix(' ')?;
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            if flags.len() != 6 {
                return None;
            }
            Some(parts.next()?.to_owned())
        })
        .collect();
    encoders.sort_unstable();
    encoders.dedup();
    encoders
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_distribution_version_banners() {
        assert_eq!(
            parse_version("ffmpeg version 6.1.1 Copyright (c) 2000-2023"),
            Some((6, 1))
        );
        assert_eq!(parse_version("ffmpeg version n7.0 Copyright"), Some((7, 0)));
        assert_eq!(
            parse_version("ffmpeg version 5.1.4-0+deb12u1 Copyright"),
            Some((5, 1))
        );
        assert_eq!(parse_version("ffmpeg version 4 Copyright"), Some((4, 0)));
    }

    #[test]
    fn rejects_a_banner_with_no_version() {
        assert_eq!(parse_version("something else entirely"), None);
        // Self-built binaries sometimes report only a git hash.
        assert_eq!(parse_version("ffmpeg version git-2024-01-01 Copyright"), None);
    }

    #[test]
    fn minimum_version_is_the_one_that_introduced_fps_mode() {
        assert!(MINIMUM_FFMPEG_VERSION >= (5, 1));
    }
}
