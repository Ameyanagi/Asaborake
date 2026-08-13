//! Encoding profiles.
//!
//! A profile is the whole of what an operator normally wants to change:
//! which encoder, at what quality, to what container, at what resolution.
//! Everything else about the pipeline — how the cuts are applied, how chapters
//! are written, how progress is reported — is fixed, because getting those
//! wrong produces broken output rather than differently-shaped output.
//!
//! Profiles are TOML, so a deployment can add one without rebuilding.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Output container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    /// MP4, which every player and every browser handles.
    Mp4,
    /// Matroska, for when chapters and multiple audio tracks matter more than
    /// universal playback.
    Mkv,
}

impl Container {
    /// The file extension, including the dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => ".mp4",
            Self::Mkv => ".mkv",
        }
    }

    /// ffmpeg's name for the muxer.
    #[must_use]
    pub const fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "matroska",
        }
    }
}

/// Video encoding settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoSettings {
    /// ffmpeg encoder name, e.g. `h264_nvenc`.
    pub encoder: String,
    /// Encoder arguments, passed through verbatim.
    #[serde(default)]
    pub args: Vec<String>,
    /// Scale down to at most this height, keeping the aspect ratio.
    ///
    /// Japanese HD broadcast is 1440x1080 with a 16:9 display aspect, so the
    /// scale filter must compute width from height rather than the reverse.
    #[serde(default)]
    pub max_height: Option<u32>,
}

/// Audio encoding settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
    /// ffmpeg encoder name, e.g. `aac`.
    pub encoder: String,
    /// Encoder arguments, passed through verbatim.
    #[serde(default)]
    pub args: Vec<String>,
    /// Output channel count.
    #[serde(default = "default_channels")]
    pub channels: u32,
    /// Output sample rate.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    /// Which side of a dual-mono programme to keep.
    ///
    /// Japanese broadcast carries bilingual audio as dual mono; without this
    /// ffmpeg merges both languages into one track and the result is
    /// unwatchable.
    #[serde(default = "default_dual_mono")]
    pub dual_mono_mode: String,
}

const fn default_channels() -> u32 {
    2
}
const fn default_sample_rate() -> u32 {
    48_000
}
fn default_dual_mono() -> String {
    "main".to_owned()
}

/// Filtering applied before encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSettings {
    /// Deinterlacer to apply to interlaced sources, or `None` to leave alone.
    #[serde(default = "default_deinterlace")]
    pub deinterlace: Option<String>,
    /// Extra filters, appended to the chain.
    #[serde(default)]
    pub extra: Vec<String>,
}

fn default_deinterlace() -> Option<String> {
    // Broadcast is 1080i; bwdif is yadif's successor and keeps more of the
    // fine detail that survives a downscale.
    Some("bwdif=mode=send_frame:parity=auto:deint=all".to_owned())
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            deinterlace: default_deinterlace(),
            extra: Vec::new(),
        }
    }
}

/// A complete encoding profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Name used to select the profile.
    pub name: String,
    /// What the profile is for, shown in the web UI.
    #[serde(default)]
    pub description: String,
    /// Output container.
    pub container: Container,
    /// Video settings.
    pub video: VideoSettings,
    /// Audio settings.
    pub audio: AudioSettings,
    /// Filter settings.
    #[serde(default)]
    pub filters: FilterSettings,
}

impl Profile {
    /// Parse a profile from TOML.
    ///
    /// # Errors
    /// Returns [`Error::ProfileParse`] if the document is malformed.
    pub fn from_toml(text: &str) -> Result<Self, Error> {
        toml::from_str(text).map_err(|source| Error::ProfileParse {
            source: Box::new(source),
        })
    }

    /// Render back to TOML, for the profile editor.
    ///
    /// # Errors
    /// Returns [`Error::ProfileEncode`] if serialisation fails.
    pub fn to_toml(&self) -> Result<String, Error> {
        toml::to_string_pretty(self).map_err(|source| Error::ProfileEncode {
            source: Box::new(source),
        })
    }

    /// Whether the ffmpeg build can run this profile.
    ///
    /// Checked before a job starts so a missing NVENC build fails immediately
    /// rather than after the analysis pass has already burned a few minutes.
    #[must_use]
    pub fn is_supported_by(&self, ffmpeg: &asaborake_media::Ffmpeg) -> bool {
        ffmpeg.has_encoder(&self.video.encoder) && ffmpeg.has_encoder(&self.audio.encoder)
    }

    /// The video filter chain, given whether the source is interlaced.
    #[must_use]
    pub fn video_filters(&self, interlaced: bool) -> Vec<String> {
        let mut chain = Vec::new();
        if interlaced && let Some(deinterlace) = &self.filters.deinterlace {
            chain.push(deinterlace.clone());
        }
        if let Some(height) = self.video.max_height {
            // `-2` keeps the width even, which every codec requires, and
            // `min` leaves smaller sources alone rather than upscaling them.
            chain.push(format!("scale=-2:'min({height},ih)'"));
        }
        chain.extend(self.filters.extra.iter().cloned());
        chain
    }
}

/// The profiles Asaborake ships with.
///
/// NVENC first, because that is what the hardware in a recording box has and
/// what leaves the CPU free for the analysis pass; the software profiles exist
/// so the pipeline runs on a laptop and in CI, where there is no GPU.
#[must_use]
pub fn builtin() -> BTreeMap<String, Profile> {
    let mut profiles = BTreeMap::new();
    for text in [
        include_str!("../profiles/nvenc-h264.toml"),
        include_str!("../profiles/nvenc-hevc.toml"),
        include_str!("../profiles/x264-cpu.toml"),
        include_str!("../profiles/x265-cpu.toml"),
    ] {
        // These are compiled in from the repository, so a parse failure is a
        // build-time mistake; skipping keeps a bad one from taking the whole
        // set down at runtime.
        match Profile::from_toml(text) {
            Ok(profile) => {
                profiles.insert(profile.name.clone(), profile);
            }
            Err(error) => tracing::error!(%error, "built-in profile failed to parse"),
        }
    }
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_profile_parses() {
        let profiles = builtin();
        assert_eq!(profiles.len(), 4, "got {:?}", profiles.keys());
        for name in ["nvenc-h264", "nvenc-hevc", "x264-cpu", "x265-cpu"] {
            assert!(profiles.contains_key(name), "missing {name}");
        }
    }

    #[test]
    fn builtin_profiles_round_trip_through_toml() {
        for (name, profile) in builtin() {
            let text = profile.to_toml().expect("serialises");
            let parsed = Profile::from_toml(&text).expect("re-parses");
            assert_eq!(parsed, profile, "{name} did not round-trip");
        }
    }

    #[test]
    fn deinterlacing_only_applies_to_interlaced_sources() {
        let profile = builtin().remove("x264-cpu").expect("the cpu profile");
        assert!(
            profile.video_filters(true).iter().any(|f| f.contains("bwdif")),
            "interlaced sources must be deinterlaced"
        );
        assert!(
            !profile.video_filters(false).iter().any(|f| f.contains("bwdif")),
            "progressive sources must be left alone"
        );
    }

    #[test]
    fn scaling_never_upscales_and_keeps_the_width_even() {
        let profile = Profile {
            name: "test".into(),
            description: String::new(),
            container: Container::Mp4,
            video: VideoSettings {
                encoder: "libx264".into(),
                args: Vec::new(),
                max_height: Some(720),
            },
            audio: AudioSettings {
                encoder: "aac".into(),
                args: Vec::new(),
                channels: 2,
                sample_rate: 48_000,
                dual_mono_mode: "main".into(),
            },
            filters: FilterSettings {
                deinterlace: None,
                extra: Vec::new(),
            },
        };
        let chain = profile.video_filters(false);
        assert_eq!(chain, vec!["scale=-2:'min(720,ih)'".to_owned()]);
    }

    #[test]
    fn extra_filters_come_last() {
        let mut profile = builtin().remove("x264-cpu").expect("the cpu profile");
        profile.filters.extra = vec!["hqdn3d".to_owned()];
        let chain = profile.video_filters(true);
        assert_eq!(chain.last().map(String::as_str), Some("hqdn3d"));
    }

    #[test]
    fn containers_know_their_extension_and_muxer() {
        assert_eq!(Container::Mp4.extension(), ".mp4");
        assert_eq!(Container::Mkv.muxer(), "matroska");
    }

    #[test]
    fn a_malformed_profile_is_rejected() {
        assert!(Profile::from_toml("this is not toml = = =").is_err());
        // Valid TOML, but missing the required fields.
        assert!(Profile::from_toml("name = \"x\"").is_err());
    }

    #[test]
    fn dual_mono_defaults_to_the_main_language() {
        let profile = builtin().remove("nvenc-h264").expect("the nvenc profile");
        assert_eq!(profile.audio.dual_mono_mode, "main");
    }
}
