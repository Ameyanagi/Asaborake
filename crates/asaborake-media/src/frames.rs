//! Streaming decoded luma frames out of ffmpeg over a pipe.
//!
//! Logo learning and detection, and scene-change detection, all work on the
//! luma plane alone, so the reader asks ffmpeg for `gray` and nothing else.
//! At 1440x1080 that is 1.5 MB per frame — around 47 MB/s at broadcast rates,
//! which a local pipe absorbs without noticing, and it avoids linking libav.
//!
//! The frame index must map exactly onto a timestamp, because a cut placed one
//! frame out is a cut placed in the wrong place. That is why the reader pins
//! the frame rate rather than trusting the source's timestamps.

use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};

use crate::Error;
use crate::ffmpeg::Ffmpeg;
use crate::probe::MediaProbe;
use crate::run::StderrTail;

/// How the analysis pass should decode.
#[derive(Debug, Clone, Default)]
pub struct FrameReaderOptions {
    /// Scale frames to this size before returning them.
    ///
    /// Logo work needs source resolution; scene detection does not. Leaving
    /// this unset and downscaling in Rust keeps both to a single decode.
    pub scale: Option<(u32, u32)>,

    /// Deinterlace before returning frames.
    ///
    /// Broadcast 1080i comb artefacts look like motion to a scene-change
    /// detector, so this is on for any interlaced source.
    pub deinterlace: bool,

    /// Return only every *n*-th frame.
    ///
    /// Logo *learning* converges long before it has seen every frame of a
    /// half-hour programme, so it decimates; detection never does, because it
    /// needs the exact frame a logo appears on.
    pub select_every: u32,

    /// Start this many seconds into the recording.
    pub start_seconds: Option<f64>,

    /// Stop after this many seconds of the recording.
    pub duration_seconds: Option<f64>,

    /// Hardware decoder to request, e.g. `cuda`.
    ///
    /// Frames still come back over the pipe in system memory, so this trades
    /// GPU decode for a download per frame; it wins on H.264 and loses on
    /// MPEG-2, which is why it is off by default.
    pub hwaccel: Option<String>,
}

/// One decoded frame, borrowing the reader's buffer.
#[derive(Debug)]
pub struct Frame<'a> {
    /// Zero-based index in the sequence this reader produced.
    pub index: u64,
    /// Position in the recording, in seconds.
    pub timestamp: f64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Luma plane, one byte per pixel, row-major with no padding.
    pub luma: &'a [u8],
}

impl Frame<'_> {
    /// Sample the luma plane at a pixel, or `None` when out of bounds.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.luma.get((y * self.width + x) as usize).copied()
    }

    /// Box-downscale into `out`, which must be `width * height` bytes.
    ///
    /// Scene-change detection wants a small frame; doing the reduction here
    /// rather than in a second ffmpeg filter keeps the pass to one decode.
    pub fn downscale_into(&self, width: u32, height: u32, out: &mut [u8]) {
        if width == 0 || height == 0 || out.len() < (width * height) as usize {
            return;
        }
        for y in 0..height {
            // Source rows covered by this destination row.
            let y0 = (y * self.height / height) as usize;
            let y1 = (((y + 1) * self.height / height) as usize).max(y0 + 1);
            for x in 0..width {
                let x0 = (x * self.width / width) as usize;
                let x1 = (((x + 1) * self.width / width) as usize).max(x0 + 1);

                let mut total = 0u32;
                let mut count = 0u32;
                for row in y0..y1.min(self.height as usize) {
                    let base = row * self.width as usize;
                    for column in x0..x1.min(self.width as usize) {
                        if let Some(&value) = self.luma.get(base + column) {
                            total += u32::from(value);
                            count += 1;
                        }
                    }
                }
                if let Some(slot) = out.get_mut((y * width + x) as usize) {
                    // A destination pixel covering no source pixels can only
                    // happen on a degenerate request; zero is as good an answer
                    // as any and avoids a division by zero.
                    *slot = total.checked_div(count).unwrap_or(0).min(255) as u8;
                }
            }
        }
    }
}

/// Reads decoded luma frames from an ffmpeg child process.
#[derive(Debug)]
pub struct FrameReader {
    child: Child,
    stdout: Option<BufReader<ChildStdout>>,
    stderr: StderrTail,
    width: u32,
    height: u32,
    /// Seconds advanced per returned frame, accounting for decimation.
    seconds_per_frame: f64,
    start_offset: f64,
    buffer: Vec<u8>,
    index: u64,
    finished: bool,
}

impl FrameReader {
    /// Start decoding `input`.
    ///
    /// # Errors
    /// Returns [`Error::NoVideoStream`] when the file has no video, or
    /// [`Error::Spawn`] when ffmpeg cannot be started.
    pub fn open(
        ffmpeg: &Ffmpeg,
        input: &Path,
        probe: &MediaProbe,
        options: &FrameReaderOptions,
    ) -> Result<Self, Error> {
        let video = probe.video.as_ref().ok_or_else(|| Error::NoVideoStream {
            path: input.to_path_buf(),
        })?;

        let step = options.select_every.max(1);
        let (width, height) = options.scale.unwrap_or((video.width, video.height));
        if width == 0 || height == 0 {
            return Err(Error::NoVideoStream {
                path: input.to_path_buf(),
            });
        }

        let mut command = ffmpeg.command();

        // Seeking before the input is the fast path; ffmpeg still lands on the
        // preceding keyframe, which is why the offset is tracked separately
        // rather than assumed exact.
        if let Some(start) = options.start_seconds.filter(|s| *s > 0.0) {
            command.args(["-ss", &format!("{start:.6}")]);
        }
        if let Some(hwaccel) = &options.hwaccel {
            command.args(["-hwaccel", hwaccel]);
        }
        // Corrupt packets are normal in terrestrial recordings; dropping them
        // beats aborting the analysis of an otherwise fine programme.
        command.args(["-fflags", "+discardcorrupt"]);
        command.arg("-i").arg(input);
        if let Some(duration) = options.duration_seconds.filter(|d| *d > 0.0) {
            command.args(["-t", &format!("{duration:.6}")]);
        }

        command.args(["-map", "0:v:0", "-an", "-sn", "-dn"]);
        command.args(["-vf", &build_filter(options, step)]);

        // With no decimation, pinning to CFR makes index-to-timestamp exact
        // even when the source's own timestamps stutter. With decimation the
        // frames must pass through untouched, or ffmpeg would helpfully
        // duplicate them back up to the original rate.
        command.args(["-fps_mode", if step > 1 { "passthrough" } else { "cfr" }]);
        command.args(["-f", "rawvideo", "-pix_fmt", "gray", "-"]);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|source| Error::Spawn {
            program: ffmpeg.ffmpeg_path().display().to_string(),
            source,
        })?;
        let stderr = StderrTail::spawn(&mut child);
        let stdout = child.stdout.take().map(BufReader::new);

        let fps = video.fps();
        let seconds_per_frame = if fps > 0.0 {
            f64::from(step) / fps
        } else {
            0.0
        };

        Ok(Self {
            child,
            stdout,
            stderr,
            width,
            height,
            seconds_per_frame,
            start_offset: options.start_seconds.unwrap_or(0.0),
            buffer: vec![0u8; (width as usize) * (height as usize)],
            index: 0,
            finished: false,
        })
    }

    /// Width of the frames this reader returns.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height of the frames this reader returns.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Seconds between consecutive returned frames.
    #[must_use]
    pub const fn seconds_per_frame(&self) -> f64 {
        self.seconds_per_frame
    }

    /// Decode the next frame, or `None` at end of stream.
    ///
    /// # Errors
    /// Returns [`Error::Failed`] when ffmpeg exited non-zero, or
    /// [`Error::Io`] when the pipe fails mid-frame.
    pub fn next_frame(&mut self) -> Result<Option<Frame<'_>>, Error> {
        if self.finished {
            return Ok(None);
        }
        let Some(stdout) = self.stdout.as_mut() else {
            self.finished = true;
            return Ok(None);
        };

        let wanted = self.buffer.len();
        let mut filled = 0usize;
        while filled < wanted {
            match stdout.read(&mut self.buffer[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(source) => return Err(Error::Io { source }),
            }
        }

        if filled < wanted {
            // A short read is end of stream. A *partial* frame means ffmpeg
            // died mid-write, which the exit status will confirm.
            if filled > 0 {
                tracing::warn!("discarding {filled} trailing bytes: ffmpeg stopped mid-frame");
            }
            self.finish()?;
            return Ok(None);
        }

        let index = self.index;
        self.index += 1;
        Ok(Some(Frame {
            index,
            timestamp: self.start_offset + index as f64 * self.seconds_per_frame,
            width: self.width,
            height: self.height,
            luma: &self.buffer,
        }))
    }

    /// Reap the child and turn a non-zero exit into an error.
    fn finish(&mut self) -> Result<(), Error> {
        self.finished = true;
        self.stdout = None;
        let status = self.child.wait().map_err(|source| Error::Io { source })?;
        self.stderr.join();
        if status.success() {
            Ok(())
        } else {
            Err(Error::Failed {
                program: "ffmpeg".to_owned(),
                code: status.code(),
                stderr: self.stderr.text(),
            })
        }
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        if !self.finished {
            // An abandoned reader must not leave ffmpeg decoding a three-hour
            // recording into a pipe nobody is reading.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Assemble the `-vf` chain for the requested options.
fn build_filter(options: &FrameReaderOptions, step: u32) -> String {
    let mut filters: Vec<String> = Vec::new();

    if options.deinterlace {
        // bwdif is yadif's successor and noticeably kinder to the fine detail
        // a logo is made of.
        filters.push("bwdif=mode=send_frame:parity=auto:deint=all".to_owned());
    }
    if step > 1 {
        filters.push(format!("select='not(mod(n\\,{step}))'"));
    }
    if let Some((width, height)) = options.scale {
        filters.push(format!("scale={width}:{height}:flags=bilinear"));
    }
    filters.push("format=gray".to_owned());

    filters.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_chain_is_minimal_by_default() {
        let options = FrameReaderOptions::default();
        assert_eq!(build_filter(&options, 1), "format=gray");
    }

    #[test]
    fn filter_chain_orders_deinterlace_before_decimation_and_scale() {
        let options = FrameReaderOptions {
            deinterlace: true,
            scale: Some((320, 180)),
            select_every: 5,
            ..FrameReaderOptions::default()
        };
        let chain = build_filter(&options, 5);
        let deinterlace = chain.find("bwdif").expect("bwdif present");
        let select = chain.find("select").expect("select present");
        let scale = chain.find("scale=").expect("scale present");
        assert!(
            deinterlace < select && select < scale,
            "unexpected order: {chain}"
        );
        assert!(chain.ends_with("format=gray"), "chain was {chain}");
    }

    #[test]
    fn decimation_escapes_the_comma_ffmpeg_would_read_as_a_separator() {
        let options = FrameReaderOptions {
            select_every: 3,
            ..FrameReaderOptions::default()
        };
        assert!(build_filter(&options, 3).contains("mod(n\\,3)"));
    }

    /// Build a frame over a borrowed buffer for the pure-arithmetic tests.
    fn frame(width: u32, height: u32, luma: &[u8]) -> Frame<'_> {
        Frame {
            index: 0,
            timestamp: 0.0,
            width,
            height,
            luma,
        }
    }

    #[test]
    fn pixel_reads_are_bounds_checked() {
        let data = [10u8, 20, 30, 40];
        let f = frame(2, 2, &data);
        assert_eq!(f.pixel(0, 0), Some(10));
        assert_eq!(f.pixel(1, 1), Some(40));
        assert_eq!(f.pixel(2, 0), None);
        assert_eq!(f.pixel(0, 2), None);
    }

    #[test]
    fn downscale_averages_each_source_block() {
        // A 4x4 frame split into four 2x2 blocks of constant value.
        let mut data = vec![0u8; 16];
        for y in 0..4usize {
            for x in 0..4usize {
                let block = (y / 2) * 2 + (x / 2);
                data[y * 4 + x] = [10u8, 20, 30, 40][block];
            }
        }
        let f = frame(4, 4, &data);

        let mut out = vec![0u8; 4];
        f.downscale_into(2, 2, &mut out);
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    #[test]
    fn downscale_ignores_an_undersized_destination() {
        let data = vec![255u8; 16];
        let f = frame(4, 4, &data);
        let mut out = vec![0u8; 2];
        f.downscale_into(2, 2, &mut out);
        assert_eq!(out, vec![0, 0], "must not write past the buffer");
    }
}
