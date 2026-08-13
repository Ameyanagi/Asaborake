//! The logo store.
//!
//! Learning a logo costs three extra decoding passes over the recording. Doing
//! that once per channel instead of once per recording is the difference
//! between an analysis that takes minutes and one that takes seconds, so the
//! result is kept.
//!
//! Logos are keyed by channel *and* frame size, because the rectangle is in
//! pixels: a channel that switches between 1440x1080 and 1920x1080 needs a
//! logo for each, and using one at the other resolution samples the wrong part
//! of the picture entirely.

use std::path::{Path, PathBuf};

use asaborake_analyze::LogoData;

use crate::Error;

/// A directory of learned logos.
#[derive(Debug, Clone)]
pub struct LogoStore {
    root: PathBuf,
}

impl LogoStore {
    /// Open a store rooted at `root`, which is created if absent.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| Error::Io {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    /// The directory being used.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The filename a logo for this channel and frame size is stored under.
    #[must_use]
    pub fn key(channel_id: &str, width: u32, height: u32) -> String {
        // Channel ids come from EPGStation and are numeric in practice, but
        // the value reaches us through an environment variable, so it is
        // sanitised rather than trusted to be path-safe.
        let safe: String = channel_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("{safe}-{width}x{height}.abl")
    }

    /// Load the logo for a channel at a frame size, if one has been learned.
    #[must_use]
    pub fn load(&self, channel_id: &str, width: u32, height: u32) -> Option<LogoData> {
        let path = self.root.join(Self::key(channel_id, width, height));
        match LogoData::load(&path) {
            Ok(logo) if logo.matches_frame_size(width, height) => Some(logo),
            Ok(logo) => {
                // Stored under one size but recorded as another: the file was
                // hand-edited or written by an older version. Ignoring it
                // means the logo is relearned, which is correct and cheap.
                tracing::warn!(
                    ?path,
                    stored = format!("{}x{}", logo.source_width, logo.source_height),
                    wanted = format!("{width}x{height}"),
                    "stored logo does not match its filename; ignoring"
                );
                None
            }
            Err(error) => {
                tracing::debug!(?path, %error, "no usable stored logo");
                None
            }
        }
    }

    /// Store a logo, replacing any previous one for the same key.
    ///
    /// # Errors
    /// Returns [`Error::Analyze`] if the logo cannot be serialised, or
    /// [`Error::Io`] if it cannot be written.
    pub fn save(&self, logo: &LogoData) -> Result<PathBuf, Error> {
        let channel = logo.channel_id.as_deref().unwrap_or("unknown");
        let path = self
            .root
            .join(Self::key(channel, logo.source_width, logo.source_height));
        logo.save(&path).map_err(Error::Analyze)?;
        tracing::info!(?path, alpha = logo.mean_alpha(), "stored logo");
        Ok(path)
    }

    /// Every logo in the store, in filename order.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the directory cannot be read.
    pub fn list(&self) -> Result<Vec<LogoData>, Error> {
        let entries = std::fs::read_dir(&self.root).map_err(|source| Error::Io {
            path: self.root.clone(),
            source,
        })?;

        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "abl"))
            .collect();
        paths.sort();

        Ok(paths
            .iter()
            // A single corrupt file must not hide the rest of the store.
            .filter_map(|path| match LogoData::load(path) {
                Ok(logo) => Some(logo),
                Err(error) => {
                    tracing::warn!(?path, %error, "skipping unreadable logo");
                    None
                }
            })
            .collect())
    }

    /// Delete the logo for a channel and frame size.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file exists but cannot be removed.
    pub fn remove(&self, channel_id: &str, width: u32, height: u32) -> Result<bool, Error> {
        let path = self.root.join(Self::key(channel_id, width, height));
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
    use asaborake_analyze::Rect;

    fn logo(channel: &str, width: u32, height: u32) -> LogoData {
        let rect = Rect {
            x: 4,
            y: 4,
            width: 16,
            height: 16,
        };
        LogoData {
            name: format!("channel {channel}"),
            channel_id: Some(channel.to_owned()),
            source_width: width,
            source_height: height,
            rect,
            a: vec![2.0; rect.area()],
            b: vec![-0.9; rect.area()],
            frames_used: 400,
        }
    }

    #[test]
    fn a_stored_logo_comes_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LogoStore::open(dir.path()).expect("store opens");

        assert!(store.load("3239123", 1440, 1080).is_none());
        store.save(&logo("3239123", 1440, 1080)).expect("saves");

        let loaded = store.load("3239123", 1440, 1080).expect("loads");
        assert_eq!(loaded.channel_id.as_deref(), Some("3239123"));
        assert_eq!(loaded.source_width, 1440);
    }

    #[test]
    fn logos_are_kept_separately_per_resolution() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LogoStore::open(dir.path()).expect("store opens");

        store.save(&logo("101", 1440, 1080)).expect("saves hd");
        store.save(&logo("101", 720, 480)).expect("saves sd");

        // A channel that switches resolution needs a logo for each, and asking
        // for one it has not learned must miss rather than return the other.
        assert!(store.load("101", 1440, 1080).is_some());
        assert!(store.load("101", 720, 480).is_some());
        assert!(store.load("101", 1920, 1080).is_none());
        assert_eq!(store.list().expect("lists").len(), 2);
    }

    #[test]
    fn a_channel_id_cannot_escape_the_store_directory() {
        // The id arrives through an environment variable, so it is sanitised
        // rather than trusted.
        let key = LogoStore::key("../../etc/passwd", 1440, 1080);
        assert!(!key.contains('/'), "{key}");
        assert!(!key.contains(".."), "{key}");
        assert_eq!(key, "______etc_passwd-1440x1080.abl");
    }

    #[test]
    fn saving_twice_replaces_rather_than_accumulates() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LogoStore::open(dir.path()).expect("store opens");

        store.save(&logo("55", 1440, 1080)).expect("saves");
        let mut updated = logo("55", 1440, 1080);
        updated.frames_used = 9999;
        store.save(&updated).expect("saves again");

        let all = store.list().expect("lists");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].frames_used, 9999);
    }

    #[test]
    fn removing_reports_whether_anything_was_there() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LogoStore::open(dir.path()).expect("store opens");

        assert!(!store.remove("77", 1440, 1080).expect("removes nothing"));
        store.save(&logo("77", 1440, 1080)).expect("saves");
        assert!(store.remove("77", 1440, 1080).expect("removes"));
        assert!(store.load("77", 1440, 1080).is_none());
    }

    #[test]
    fn a_corrupt_file_does_not_hide_the_rest_of_the_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LogoStore::open(dir.path()).expect("store opens");
        store.save(&logo("1", 1440, 1080)).expect("saves");
        std::fs::write(dir.path().join("broken-1440x1080.abl"), b"not a logo")
            .expect("writes junk");

        assert_eq!(store.list().expect("lists").len(), 1);
    }
}
