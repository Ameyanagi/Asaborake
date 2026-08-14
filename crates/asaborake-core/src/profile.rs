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
use std::path::PathBuf;

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

    /// Whether this container can carry an audio codec as-is.
    ///
    /// Broadcast audio is AAC and both containers take it, so the usual answer
    /// is yes — which is what makes copying rather than re-encoding possible.
    #[must_use]
    pub fn can_carry(self, codec: &str) -> bool {
        match self {
            // MP4 is fussy: AAC and the MPEG audio family, and little else
            // that a broadcast recording would contain.
            Self::Mp4 => matches!(codec, "aac" | "mp3" | "ac3" | "eac3"),
            // Matroska takes essentially anything.
            Self::Mkv => true,
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

/// The deinterlacer a profile uses unless it says otherwise.
///
/// Broadcast is 1080i; bwdif is yadif's successor and keeps more of the fine
/// detail that survives a downscale. Wrapped in an `Option` because the field
/// it defaults is optional: a profile may set it to nothing to leave
/// interlaced sources alone.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde default for an Option field"
)]
fn default_deinterlace() -> Option<String> {
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

/// Profiles a deployment has added or changed, on top of the shipped ones.
///
/// Kept as TOML files in a directory because a profile *is* a TOML document —
/// the format the engine already parses, and the one an operator can read,
/// copy and mail to somebody. Editing one in a browser and editing one with a
/// text editor stay the same act.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    /// Open the store at `root`, which need not exist.
    #[must_use]
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The file a profile is kept in.
    ///
    /// The name arrives over HTTP, so it is sanitised rather than trusted to
    /// stay inside the directory.
    fn path(&self, name: &str) -> PathBuf {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root.join(format!("{safe}.toml"))
    }

    /// Every profile: the shipped ones, with any stored ones replacing or
    /// adding to them by name.
    #[must_use]
    pub fn all(&self) -> BTreeMap<String, Profile> {
        let mut profiles = builtin();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return profiles;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match Profile::from_toml(&text) {
                Ok(profile) => {
                    profiles.insert(profile.name.clone(), profile);
                }
                // One unreadable file must not hide the rest, and the engine
                // still has the shipped profiles to work with.
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "cannot read this profile");
                }
            }
        }
        profiles
    }

    /// Store a profile, replacing any of the same name.
    ///
    /// # Errors
    /// Returns [`Error::ProfileParse`] if the document is malformed, or
    /// [`Error::Io`] if it cannot be written.
    pub fn save(&self, toml: &str) -> Result<Profile, Error> {
        // Parsed before it is written, so a document that would break the
        // engine is refused while somebody is still looking at it.
        let profile = Profile::from_toml(toml)?;
        std::fs::create_dir_all(&self.root).map_err(|source| Error::Io {
            path: self.root.clone(),
            source,
        })?;
        let path = self.path(&profile.name);
        std::fs::write(&path, toml).map_err(|source| Error::Io { path, source })?;
        Ok(profile)
    }

    /// Remove a stored profile, returning whether there was one.
    ///
    /// A shipped profile cannot be removed; forgetting an override restores
    /// whatever it was overriding.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file exists and cannot be removed.
    pub fn remove(&self, name: &str) -> Result<bool, Error> {
        let path = self.path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io { path, source }),
        }
    }
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
            profile
                .video_filters(true)
                .iter()
                .any(|f| f.contains("bwdif")),
            "interlaced sources must be deinterlaced"
        );
        assert!(
            !profile
                .video_filters(false)
                .iter()
                .any(|f| f.contains("bwdif")),
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
    fn a_stored_profile_joins_the_shipped_ones() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ProfileStore::open(dir.path());
        assert_eq!(store.all().len(), builtin().len());

        let mut custom = builtin().remove("x264-cpu").expect("profile");
        custom.name = "my-profile".to_owned();
        store
            .save(&custom.to_toml().expect("serialises"))
            .expect("saves");

        let all = store.all();
        assert_eq!(all.len(), builtin().len() + 1);
        assert!(all.contains_key("my-profile"));
        // And the shipped ones are still there.
        assert!(all.contains_key("x264-cpu"));
    }

    #[test]
    fn a_stored_profile_can_replace_a_shipped_one_and_be_taken_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ProfileStore::open(dir.path());

        let mut changed = builtin().remove("x264-cpu").expect("profile");
        changed.description = "mine".to_owned();
        store
            .save(&changed.to_toml().expect("serialises"))
            .expect("saves");
        assert_eq!(store.all()["x264-cpu"].description, "mine");

        // Forgetting the override restores what it was overriding, rather
        // than leaving the engine without that profile.
        assert!(store.remove("x264-cpu").expect("removes"));
        assert_ne!(store.all()["x264-cpu"].description, "mine");
        assert!(!store.remove("x264-cpu").expect("removes"), "already gone");
    }

    #[test]
    fn a_document_that_would_break_the_engine_is_refused_before_it_is_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ProfileStore::open(dir.path());

        assert!(store.save("this is not a profile").is_err());
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("reads").count(),
            0,
            "nothing should have been written"
        );
    }

    #[test]
    fn a_profile_name_cannot_escape_the_store_directory() {
        // The name comes from inside the document, which arrives over HTTP.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ProfileStore::open(dir.path());

        let mut nasty = builtin().remove("x264-cpu").expect("profile");
        nasty.name = "../../etc/passwd".to_owned();
        store
            .save(&nasty.to_toml().expect("serialises"))
            .expect("saves");

        let written: Vec<_> = std::fs::read_dir(dir.path())
            .expect("reads")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(written, vec!["______etc_passwd.toml"], "{written:?}");
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
