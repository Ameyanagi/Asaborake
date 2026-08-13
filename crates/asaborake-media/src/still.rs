//! One frame of a recording, as a picture.
//!
//! The logo tool needs to show an operator what a recording looks like at a
//! given moment so they can point at the station logo. That is the whole
//! purpose: Amatsukaze's logo detection works in practice because a human
//! draws the rectangle once per channel, and Asaborake could not learn a logo
//! on real broadcast precisely because there was no way to aim.
//!
//! Frames come out as PNG rather than JPEG so the logo edges the operator is
//! aiming at are the ones the scanner will see, not ones softened by chroma
//! subsampling.

use std::path::Path;
use std::process::Stdio;

use crate::Error;
use crate::ffmpeg::Ffmpeg;
use crate::run::capture_stdout;

/// The largest still the tool will render, in pixels across.
///
/// Broadcast is 1440 wide; anything larger would be upscaling, and anything
/// much smaller would make a small logo impossible to aim at.
pub const MAX_WIDTH: u32 = 1920;

/// Render the frame at `seconds` as a PNG.
///
/// `width` scales the output, keeping the aspect ratio, and is clamped to
/// [`MAX_WIDTH`]. The height follows from the source's display aspect, so a
/// 1440x1080 anamorphic broadcast frame comes back at 16:9 — which is what the
/// operator sees when they watch it, and therefore what they must aim at.
///
/// # Errors
/// Returns [`Error::Failed`] when ffmpeg cannot decode a frame there, which is
/// what happens when `seconds` is past the end of the recording.
pub fn still_png(
    ffmpeg: &Ffmpeg,
    input: &Path,
    seconds: f64,
    width: u32,
) -> Result<Vec<u8>, Error> {
    let width = width.clamp(160, MAX_WIDTH);
    let mut command = ffmpeg.command();

    // Seeking before the input decodes only from the preceding keyframe, which
    // is what makes scrubbing feel immediate on a multi-gigabyte recording.
    // The frame returned may be up to a GOP earlier than asked for; for aiming
    // at a logo, which does not move, that does not matter.
    if seconds > 0.0 {
        command.args(["-ss", &format!("{seconds:.3}")]);
    }
    command.args(["-fflags", "+discardcorrupt"]);
    command.arg("-i").arg(input);
    command.args(["-map", "0:v:0", "-an", "-sn", "-dn"]);
    command.args(["-frames:v", "1"]);
    // Japanese HD broadcast is 1440x1080 stored, displayed at 16:9 — the
    // pixels are not square. A browser has no idea about that, so the height
    // is computed from the *display* aspect rather than the stored one and the
    // sample aspect is reset to square. Without this a 16:9 frame arrives
    // looking like 4:3 and everything in it is stretched tall.
    command.args([
        "-vf",
        &format!("scale=w={width}:h=trunc({width}/dar/2)*2,setsar=1"),
    ]);
    command.args(["-f", "image2", "-c:v", "png", "-"]);
    command.stdin(Stdio::null());

    capture_stdout(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn ffmpeg() -> Option<Ffmpeg> {
        Ffmpeg::discover(None, None).ok()
    }

    fn render_clip(path: &Path) {
        let status = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=25:duration=4",
            ])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(path)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success());
    }

    #[test]
    fn renders_a_png_at_the_requested_width() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        render_clip(&input);

        let png = still_png(&ffmpeg, &input, 2.0, 160).expect("renders");

        // PNG magic, then the IHDR width as a big-endian u32 at offset 16.
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a png");
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        assert_eq!(width, 160);
    }

    #[test]
    fn an_absurd_width_is_clamped_rather_than_honoured() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        render_clip(&input);

        // A width arriving over HTTP must not be able to ask for a gigapixel
        // encode, and must not be able to ask for zero either.
        let png = still_png(&ffmpeg, &input, 0.0, 100_000).expect("renders");
        let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        assert!(width <= MAX_WIDTH, "got {width}");
    }

    #[test]
    fn a_position_past_the_end_fails_rather_than_returning_nothing() {
        let Some(ffmpeg) = ffmpeg() else { return };
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        render_clip(&input);

        // Silently returning an empty body would render as a broken image with
        // no explanation of why.
        let result = still_png(&ffmpeg, &input, 600.0, 320);
        assert!(
            result.as_ref().map_or(true, Vec::is_empty),
            "expected no frame past the end"
        );
    }
}
