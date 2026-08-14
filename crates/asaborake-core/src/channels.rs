//! What to do differently for a particular channel.
//!
//! A recording box watches the same dozen channels for years, and they are not
//! alike. NHK carries no advertising at all, so looking for commercial breaks
//! in it spends an analysis pass to find nothing and risks cutting a
//! programme. A film channel wants a better profile than a shopping channel.
//! Amatsukaze has a per-channel settings panel and a rule engine for exactly
//! this; the useful half of it is a handful of settings keyed by channel.
//!
//! Kept as one file rather than a database table because it is a dozen rows
//! that a person may reasonably want to edit by hand, and because the engine
//! must start with sensible behaviour when it does not exist.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Error;

/// How one channel should be treated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSettings {
    /// Human-readable name, for the settings screen.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether to look for commercials at all.
    ///
    /// Off for a channel that carries none: the analysis pass is skipped
    /// rather than run to conclude nothing, and the recording is transcoded
    /// whole. That is both faster and safer than trusting a detector to find
    /// nothing in material that contains nothing.
    #[serde(default = "yes")]
    pub detect_commercials: bool,
    /// Encoding profile to use instead of the one the job asked for.
    #[serde(default)]
    pub profile: Option<String>,
}

const fn yes() -> bool {
    true
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self {
            name: None,
            detect_commercials: true,
            profile: None,
        }
    }
}

/// Per-channel settings, as a file.
#[derive(Debug, Clone)]
pub struct ChannelStore {
    path: PathBuf,
}

impl ChannelStore {
    /// Open the store at `path`, which need not exist yet.
    #[must_use]
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Every channel that has settings, keyed by channel id.
    ///
    /// A file that cannot be read or parsed yields no settings rather than an
    /// error: the defaults are what the engine did before this existed, and a
    /// typo in a hand-edited file should not stop the queue.
    #[must_use]
    pub fn all(&self) -> BTreeMap<String, ChannelSettings> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return BTreeMap::new();
        };
        match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, path = %self.path.display(), "cannot read channel settings");
                BTreeMap::new()
            }
        }
    }

    /// The settings for one channel, or the defaults.
    #[must_use]
    pub fn get(&self, channel_id: &str) -> ChannelSettings {
        self.all().remove(channel_id).unwrap_or_default()
    }

    /// Replace one channel's settings.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file cannot be written, or
    /// [`Error::SidecarEncode`] if the settings cannot be serialised.
    pub fn set(&self, channel_id: &str, settings: &ChannelSettings) -> Result<(), Error> {
        let mut all = self.all();
        all.insert(channel_id.to_owned(), settings.clone());
        self.write(&all)
    }

    /// Forget one channel's settings, returning whether there were any.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file cannot be written.
    pub fn remove(&self, channel_id: &str) -> Result<bool, Error> {
        let mut all = self.all();
        let existed = all.remove(channel_id).is_some();
        if existed {
            self.write(&all)?;
        }
        Ok(existed)
    }

    fn write(&self, all: &BTreeMap<String, ChannelSettings>) -> Result<(), Error> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_string_pretty(all).map_err(Error::SidecarEncode)?;
        std::fs::write(&self.path, json).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (ChannelStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        (ChannelStore::open(dir.path().join("channels.json")), dir)
    }

    #[test]
    fn a_channel_nobody_configured_gets_the_old_behaviour() {
        // Which is: look for commercials, and use whatever profile the job
        // asked for.
        let (store, _dir) = store();
        let settings = store.get("1024");

        assert!(settings.detect_commercials);
        assert_eq!(settings.profile, None);
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let (store, _dir) = store();
        store
            .set(
                "1024",
                &ChannelSettings {
                    name: Some("NHK総合".to_owned()),
                    detect_commercials: false,
                    profile: Some("nvenc-hevc".to_owned()),
                },
            )
            .expect("writes");

        let read = store.get("1024");
        assert!(!read.detect_commercials);
        assert_eq!(read.profile.as_deref(), Some("nvenc-hevc"));
        assert_eq!(read.name.as_deref(), Some("NHK総合"));

        // And one channel's settings do not become another's.
        assert!(store.get("1040").detect_commercials);
    }

    #[test]
    fn one_channel_can_be_changed_without_disturbing_the_rest() {
        let (store, _dir) = store();
        store
            .set("1024", &ChannelSettings::default())
            .expect("writes");
        store
            .set(
                "1040",
                &ChannelSettings {
                    detect_commercials: false,
                    ..ChannelSettings::default()
                },
            )
            .expect("writes");

        assert_eq!(store.all().len(), 2);
        assert!(store.get("1024").detect_commercials);
        assert!(!store.get("1040").detect_commercials);

        assert!(store.remove("1024").expect("removes"));
        assert!(!store.remove("1024").expect("removes"), "already gone");
        assert_eq!(store.all().len(), 1);
    }

    #[test]
    fn a_hand_edited_file_that_will_not_parse_does_not_stop_the_queue() {
        // The defaults are what the engine did before this existed, so falling
        // back to them is safe; refusing to run is not.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("channels.json");
        std::fs::write(&path, "{ this is not json").expect("writes");

        let store = ChannelStore::open(&path);
        assert!(store.all().is_empty());
        assert!(store.get("1024").detect_commercials);
    }
}
