//! Which recordings the logo tool is allowed to look at.
//!
//! The tool serves frames out of recordings so a browser can show what one
//! looks like. That means a path arrives over HTTP and is handed to ffmpeg,
//! which is exactly the shape of a directory-traversal hole: `../../etc/shadow`
//! is a perfectly good path. Nothing outside the configured directories is
//! readable, and the check is done on the *canonical* path so a symlink cannot
//! be used to step outside one either.

use std::path::{Path, PathBuf};

/// Extensions the tool will offer. Anything else is not a recording.
const RECORDING_EXTENSIONS: &[&str] = &["ts", "m2ts", "mts", "tsv", "mp4", "mkv"];

/// One recording an operator can pick.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Recording {
    /// Absolute path, which is what the frame and scan endpoints take back.
    pub path: String,
    /// Just the file name, for display.
    pub name: String,
    /// Size in bytes, so an obviously truncated recording is visible.
    pub size: u64,
}

/// List the recordings under `roots`, newest first.
///
/// One level deep only: a recordings directory is flat in every setup this
/// targets, and walking arbitrarily deep turns one stray symlink into an
/// unbounded traversal.
#[must_use]
pub fn list(roots: &[PathBuf]) -> Vec<Recording> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            tracing::warn!(path = %root.display(), "cannot read the recordings directory");
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_recording(&path) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            found.push((
                metadata.modified().ok(),
                Recording {
                    name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path: path.to_string_lossy().into_owned(),
                    size: metadata.len(),
                },
            ));
        }
    }

    // Newest first: the recording someone wants to aim at is almost always the
    // one that just finished.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    found.into_iter().map(|(_, recording)| recording).collect()
}

/// Whether the path looks like something worth offering.
fn is_recording(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| RECORDING_EXTENSIONS.contains(&e.as_str()))
}

/// Resolve a path the client asked for, or refuse it.
///
/// Returns the canonical path when it lies inside one of `roots`, and `None`
/// otherwise — including when it does not exist, because reporting the
/// difference would let a caller probe the filesystem for what is there.
#[must_use]
pub fn resolve(roots: &[PathBuf], requested: &str) -> Option<PathBuf> {
    // Canonicalising is what defeats both `..` and symlinks: it resolves them
    // before the comparison rather than after.
    let path = std::fs::canonicalize(requested).ok()?;
    if !path.is_file() || !is_recording(&path) {
        return None;
    }
    for root in roots {
        let Ok(root) = std::fs::canonicalize(root) else {
            continue;
        };
        if path.starts_with(&root) {
            return Some(path);
        }
    }
    tracing::warn!(path = %path.display(), "refused a path outside the recording directories");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_recordings_and_ignores_everything_else() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["a.ts", "b.mp4", "notes.txt", "c.mkv"] {
            std::fs::write(dir.path().join(name), b"x").expect("writes");
        }
        std::fs::create_dir(dir.path().join("subdir")).expect("creates");

        let roots = vec![dir.path().to_path_buf()];
        let found = list(&roots);
        let names: Vec<&str> = found.iter().map(|r| r.name.as_str()).collect();

        assert_eq!(names.len(), 3, "{names:?}");
        assert!(!names.contains(&"notes.txt"), "{names:?}");
        assert!(!names.contains(&"subdir"), "{names:?}");
    }

    #[test]
    fn a_path_inside_a_recording_directory_resolves() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("show.ts");
        std::fs::write(&file, b"x").expect("writes");

        let roots = vec![dir.path().to_path_buf()];
        assert!(resolve(&roots, &file.to_string_lossy()).is_some());
    }

    #[test]
    fn a_path_outside_the_recording_directories_is_refused() {
        let allowed = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let secret = elsewhere.path().join("secret.ts");
        std::fs::write(&secret, b"x").expect("writes");

        let roots = vec![allowed.path().to_path_buf()];
        assert_eq!(resolve(&roots, &secret.to_string_lossy()), None);
    }

    #[test]
    fn a_traversal_out_of_a_recording_directory_is_refused() {
        // The whole reason this module exists: `path` arrives over HTTP.
        let allowed = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let secret = elsewhere.path().join("secret.ts");
        std::fs::write(&secret, b"x").expect("writes");

        let roots = vec![allowed.path().to_path_buf()];
        let traversal = format!(
            "{}/../{}/secret.ts",
            allowed.path().display(),
            elsewhere
                .path()
                .file_name()
                .expect("a name")
                .to_string_lossy()
        );
        assert_eq!(resolve(&roots, &traversal), None);
    }

    #[test]
    fn a_symlink_pointing_out_of_a_recording_directory_is_refused() {
        // Canonicalising before the comparison is what catches this; checking
        // the requested path as written would let it straight through.
        let allowed = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let secret = elsewhere.path().join("secret.ts");
        std::fs::write(&secret, b"x").expect("writes");

        let link = allowed.path().join("innocent.ts");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, &link).expect("links");
        #[cfg(not(unix))]
        return;

        let roots = vec![allowed.path().to_path_buf()];
        assert_eq!(resolve(&roots, &link.to_string_lossy()), None);
    }

    #[test]
    fn a_file_that_is_not_a_recording_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("passwords.txt");
        std::fs::write(&file, b"x").expect("writes");

        let roots = vec![dir.path().to_path_buf()];
        assert_eq!(resolve(&roots, &file.to_string_lossy()), None);
    }

    #[test]
    fn nothing_resolves_when_no_directories_are_configured() {
        // Frame serving is off unless a deployment opts into it.
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("show.ts");
        std::fs::write(&file, b"x").expect("writes");

        assert_eq!(resolve(&[], &file.to_string_lossy()), None);
        assert!(list(&[]).is_empty());
    }
}
