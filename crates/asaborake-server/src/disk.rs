//! How much room is left where the output is going.
//!
//! An encode that runs out of disk half way through has cost an hour of GPU
//! time and leaves a truncated file that looks like a real one until somebody
//! plays it. The check is cheap and the failure is expensive, so it happens
//! before the job starts rather than being discovered during it.
//!
//! Amatsukaze monitors free space across its drives for the same reason.

use std::path::Path;
use std::process::Command;

/// Always keep this much free, whatever the recording needs.
///
/// A transport stream that changes picture size is cut into temporary slices
/// beside the output, and the database and its write-ahead log live on the
/// same disk in most deployments. Filling it to the last byte breaks more than
/// the job that did it.
const HEADROOM: u64 = 2 * 1024 * 1024 * 1024;

/// Fraction of the source a transcode is assumed to need.
///
/// Broadcast MPEG-2 to H.264 at the shipped profiles lands around a quarter of
/// the original. It is an estimate, not a measurement, so it is deliberately
/// generous — refusing a job that would have fitted is a smaller harm than
/// filling the disk, and both are announced.
const EXPECTED_SHARE: u64 = 4;

/// Free bytes on the filesystem holding `path`, when it can be determined.
///
/// Shells out to `df`, which every target has, rather than taking a
/// dependency or reaching for `statvfs` — the workspace forbids `unsafe`, and
/// this crate already drives child processes for everything else.
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    // The output file does not exist yet, so the question is about the
    // directory it will be written into.
    let directory = path.parent().unwrap_or(Path::new("."));

    // `-P` forces one line per filesystem, which is the only part of df's
    // output that is portable. `-k` fixes the unit at 1024 bytes.
    let output = Command::new("df").arg("-Pk").arg(directory).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Header, then the filesystem: capacity, used, available, ...
    let available = text.lines().nth(1)?.split_whitespace().nth(3)?;
    available.parse::<u64>().ok()?.checked_mul(1024)
}

/// Whether there is room to transcode `source` into `output`.
///
/// Returns the shortfall in bytes when there is not, and `None` when there is
/// — or when free space could not be determined, which must not stop a job:
/// an unusual filesystem is not a reason to refuse to work.
#[must_use]
pub fn shortfall(source: &Path, output: &Path) -> Option<u64> {
    let free = free_bytes(output)?;
    let source_size = std::fs::metadata(source).map_or(0, |m| m.len());
    let needed = source_size / EXPECTED_SHARE + HEADROOM;
    needed.checked_sub(free).filter(|short| *short > 0)
}

/// A size a person can read.
#[must_use]
pub fn describe(bytes: u64) -> String {
    const GIB: f64 = (1024 * 1024 * 1024) as f64;
    const MIB: f64 = (1024 * 1024) as f64;

    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_free_space_of_a_real_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let free = free_bytes(&dir.path().join("out.mp4")).expect("df reports something");
        // Any filesystem a test runs on has some room and is not infinite.
        assert!(free > 0, "reported no free space at all");
    }

    #[test]
    fn a_disk_with_room_reports_no_shortfall() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("in.ts");
        std::fs::write(&source, b"small").expect("writes");

        // A five-byte source needs the headroom and nothing more, which any
        // machine running tests has.
        assert_eq!(shortfall(&source, &dir.path().join("out.mp4")), None);
    }

    #[test]
    fn an_unreadable_location_does_not_block_a_job() {
        // Not being able to measure is not the same as not having room, and
        // refusing to work because df was unhelpful would be worse than the
        // risk it was guarding against.
        assert_eq!(
            shortfall(Path::new("/nonexistent"), Path::new("/nonexistent/out.mp4")),
            None
        );
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(describe(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(describe(512 * 1024 * 1024), "512 MiB");
    }
}
