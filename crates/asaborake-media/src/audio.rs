//! Extracting a loudness envelope from the audio track.
//!
//! Silence is one of the three signals CM boundaries are found from. Detecting
//! it needs nothing like full-quality audio — broadcast inserts a real gap of
//! near-digital-silence at block boundaries — so the audio is downmixed to
//! mono at 8 kHz. That is roughly a thousandth of the data of the original
//! track and costs almost nothing to decode.

use std::io::{BufReader, Read};
use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::ffmpeg::Ffmpeg;
use crate::run::StderrTail;

/// Sample rate the envelope is computed at.
///
/// Silence is broadband, so bandwidth buys nothing here; 8 kHz keeps a
/// three-hour recording's decode negligible next to the video pass.
pub const ENVELOPE_SAMPLE_RATE: u32 = 8_000;

/// The level reported for a window containing digital silence.
///
/// True silence is negative infinity dBFS, which serialises poorly and
/// propagates into every downstream average; a floor well below any real
/// broadcast noise level behaves better and means the same thing.
pub const SILENCE_FLOOR_DBFS: f32 = -120.0;

/// Per-window loudness across a recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RmsEnvelope {
    /// Duration each window covers, in seconds.
    pub window_seconds: f64,
    /// RMS level of each window, in dBFS, floored at [`SILENCE_FLOOR_DBFS`].
    pub windows: Vec<f32>,
}

impl RmsEnvelope {
    /// Total duration the envelope covers, in seconds.
    #[must_use]
    pub fn duration_seconds(&self) -> f64 {
        self.windows.len() as f64 * self.window_seconds
    }

    /// Start time of a window, in seconds.
    #[must_use]
    pub fn window_start(&self, index: usize) -> f64 {
        index as f64 * self.window_seconds
    }

    /// Level at a point in time, or `None` past the end.
    #[must_use]
    pub fn level_at(&self, seconds: f64) -> Option<f32> {
        if seconds < 0.0 || self.window_seconds <= 0.0 {
            return None;
        }
        self.windows
            .get((seconds / self.window_seconds) as usize)
            .copied()
    }

    /// Runs of consecutive windows quieter than `threshold_dbfs` and lasting
    /// at least `minimum_seconds`, returned as `(start, end)` in seconds.
    ///
    /// Broadcast leaves a real gap between a programme and the CM block that
    /// follows it; requiring a minimum duration is what separates that gap
    /// from an ordinary pause in dialogue.
    #[must_use]
    pub fn silent_spans(&self, threshold_dbfs: f32, minimum_seconds: f64) -> Vec<(f64, f64)> {
        let mut spans = Vec::new();
        let mut run_start: Option<usize> = None;

        for (index, &level) in self.windows.iter().enumerate() {
            if level < threshold_dbfs {
                run_start.get_or_insert(index);
            } else if let Some(start) = run_start.take() {
                self.push_span(&mut spans, start, index, minimum_seconds);
            }
        }
        if let Some(start) = run_start {
            self.push_span(&mut spans, start, self.windows.len(), minimum_seconds);
        }
        spans
    }

    fn push_span(&self, spans: &mut Vec<(f64, f64)>, start: usize, end: usize, minimum: f64) {
        let from = self.window_start(start);
        let to = self.window_start(end);
        if to - from >= minimum {
            spans.push((from, to));
        }
    }
}

/// Decode the first audio stream and compute its loudness envelope.
///
/// # Errors
/// Returns [`Error::Spawn`] if ffmpeg cannot start, or [`Error::Failed`] if it
/// exits non-zero.
pub fn rms_envelope(
    ffmpeg: &Ffmpeg,
    input: &Path,
    window_seconds: f64,
) -> Result<RmsEnvelope, Error> {
    let window_samples =
        ((f64::from(ENVELOPE_SAMPLE_RATE) * window_seconds).round() as usize).max(1);

    let mut command = ffmpeg.command();
    command.args(["-fflags", "+discardcorrupt"]);
    command.arg("-i").arg(input);
    command.args([
        "-map",
        "0:a:0",
        "-vn",
        "-sn",
        "-dn",
        "-ac",
        "1",
        "-ar",
        &ENVELOPE_SAMPLE_RATE.to_string(),
        "-f",
        "s16le",
        "-",
    ]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|source| Error::Spawn {
        program: ffmpeg.ffmpeg_path().display().to_string(),
        source,
    })?;
    let mut stderr = StderrTail::spawn(&mut child);

    let mut windows = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout);
        // Read whole windows at a time so no accumulator state has to be
        // carried across reads.
        let mut chunk = vec![0u8; window_samples * 2];
        loop {
            let mut filled = 0usize;
            while filled < chunk.len() {
                match reader.read(&mut chunk[filled..]) {
                    Ok(0) => break,
                    Ok(read) => filled += read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(source) => return Err(Error::Io { source }),
                }
            }
            if filled < 2 {
                break;
            }
            windows.push(window_level(&chunk[..filled - filled % 2]));
            if filled < chunk.len() {
                break;
            }
        }
    }

    let status = child.wait().map_err(|source| Error::Io { source })?;
    stderr.join();
    if !status.success() {
        return Err(Error::Failed {
            program: "ffmpeg".to_owned(),
            code: status.code(),
            stderr: stderr.text(),
        });
    }

    Ok(RmsEnvelope {
        window_seconds,
        windows,
    })
}

/// RMS level of one window of little-endian 16-bit samples, in dBFS.
fn window_level(bytes: &[u8]) -> f32 {
    if bytes.len() < 2 {
        return SILENCE_FLOOR_DBFS;
    }
    let mut sum_squares = 0f64;
    let mut count = 0u32;
    for pair in bytes.chunks_exact(2) {
        let sample = f64::from(i16::from_le_bytes([pair[0], pair[1]]));
        // Normalise against full scale so the result is independent of the
        // sample format.
        let normalised = sample / f64::from(i16::MAX);
        sum_squares += normalised * normalised;
        count += 1;
    }
    if count == 0 {
        return SILENCE_FLOOR_DBFS;
    }
    let rms = (sum_squares / f64::from(count)).sqrt();
    if rms <= 0.0 {
        return SILENCE_FLOOR_DBFS;
    }
    (20.0 * rms.log10()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn digital_silence_reports_the_floor() {
        let bytes = samples_to_bytes(&[0; 160]);
        assert!((window_level(&bytes) - SILENCE_FLOOR_DBFS).abs() < f32::EPSILON);
    }

    #[test]
    fn full_scale_reports_roughly_zero_dbfs() {
        // A square wave at full scale has an RMS equal to its amplitude.
        let bytes = samples_to_bytes(&[i16::MAX; 160]);
        assert!(
            window_level(&bytes).abs() < 0.01,
            "{}",
            window_level(&bytes)
        );
    }

    #[test]
    fn half_scale_is_about_minus_six_db() {
        let bytes = samples_to_bytes(&[i16::MAX / 2; 160]);
        let level = window_level(&bytes);
        assert!((level + 6.0).abs() < 0.1, "level was {level}");
    }

    fn envelope(levels: &[f32]) -> RmsEnvelope {
        RmsEnvelope {
            window_seconds: 0.02,
            windows: levels.to_vec(),
        }
    }

    #[test]
    fn finds_a_silent_span_that_meets_the_minimum_duration() {
        // 10 windows of 20 ms: quiet from window 2 to 8, i.e. 0.04s to 0.16s.
        let mut levels = vec![-20.0f32; 10];
        for level in levels.iter_mut().take(8).skip(2) {
            *level = -80.0;
        }
        let spans = envelope(&levels).silent_spans(-50.0, 0.1);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].0 - 0.04).abs() < 1e-9, "{:?}", spans[0]);
        assert!((spans[0].1 - 0.16).abs() < 1e-9, "{:?}", spans[0]);
    }

    #[test]
    fn ignores_a_pause_shorter_than_the_minimum() {
        // A two-window dip is 40 ms: an ordinary pause, not a block boundary.
        let mut levels = vec![-20.0f32; 10];
        levels[4] = -80.0;
        levels[5] = -80.0;
        assert!(envelope(&levels).silent_spans(-50.0, 0.3).is_empty());
    }

    #[test]
    fn closes_a_silent_run_that_reaches_the_end() {
        let mut levels = vec![-20.0f32; 10];
        for level in levels.iter_mut().skip(4) {
            *level = -80.0;
        }
        let spans = envelope(&levels).silent_spans(-50.0, 0.1);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].1 - 0.2).abs() < 1e-9, "{:?}", spans[0]);
    }

    #[test]
    fn level_lookup_is_bounded_by_the_envelope() {
        let e = envelope(&[-10.0, -20.0, -30.0]);
        assert_eq!(e.level_at(0.0), Some(-10.0));
        assert_eq!(e.level_at(0.03), Some(-20.0));
        assert_eq!(e.level_at(1.0), None);
        assert_eq!(e.level_at(-1.0), None);
        assert!((e.duration_seconds() - 0.06).abs() < 1e-9);
    }
}
