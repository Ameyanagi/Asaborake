//! Running ffmpeg as a child process without deadlocking on its pipes.
//!
//! ffmpeg writes far more to stderr than a pipe buffer holds. Reading stdout
//! while leaving stderr unread wedges both processes once that buffer fills,
//! so every invocation here drains stderr on its own thread and keeps the tail
//! for error reporting.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::Error;

/// How many stderr lines to keep for diagnosing a failure.
///
/// ffmpeg puts the actual cause near the end of its output, so the tail is
/// what matters; keeping the whole thing would mean holding megabytes of
/// per-frame warnings for a recording with heavy packet loss.
const STDERR_TAIL_LINES: usize = 40;

/// How far back to look for a line to fold a repeat into.
///
/// ffmpeg does not repeat one line; it cycles a small group of them, once per
/// broken frame. A window rather than "is this the previous line" is what
/// collapses that, and it is deliberately small so a genuine pattern repeating
/// across a long run is still reported once per occurrence.
const REPEAT_WINDOW: usize = 6;

/// One retained line, and how many times it was seen in a row.
#[derive(Debug)]
struct TailLine {
    text: String,
    seen: u32,
}

/// A background drain of a child's stderr that retains the last few lines.
#[derive(Debug)]
pub struct StderrTail {
    lines: Arc<Mutex<VecDeque<TailLine>>>,
    handle: Option<JoinHandle<()>>,
}

impl StderrTail {
    /// Start draining the child's stderr on a background thread.
    ///
    /// Returns a tail that captures nothing if the child has no stderr pipe,
    /// which keeps callers from having to special-case that.
    pub fn spawn(child: &mut Child) -> Self {
        let lines = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        let Some(stderr) = child.stderr.take() else {
            return Self {
                lines,
                handle: None,
            };
        };

        let sink = Arc::clone(&lines);
        let handle = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                tracing::trace!(target: "asaborake::ffmpeg", "{line}");
                let Ok(mut sink) = sink.lock() else {
                    // The mutex is only poisoned if a holder panicked; there is
                    // nothing useful left to capture, so stop draining.
                    return;
                };
                // A recording with heavy packet loss produces the same handful
                // of decoder complaints once per frame. Left alone they fill
                // the whole tail and push out the line that says what actually
                // went wrong.
                let start = sink.len().saturating_sub(REPEAT_WINDOW);
                if let Some(previous) = sink
                    .iter_mut()
                    .skip(start)
                    .find(|previous| previous.text == line)
                {
                    previous.seen = previous.seen.saturating_add(1);
                    continue;
                }

                if sink.len() == STDERR_TAIL_LINES {
                    sink.pop_front();
                }
                sink.push_back(TailLine {
                    text: line,
                    seen: 1,
                });
            }
        });

        Self {
            lines,
            handle: Some(handle),
        }
    }

    /// The retained stderr lines, joined, for inclusion in an error.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.lock().map_or_else(
            |_| String::from("(stderr unavailable)"),
            |lines| {
                lines
                    .iter()
                    .map(|line| {
                        if line.seen > 1 {
                            format!("{} (×{})", line.text, line.seen)
                        } else {
                            line.text.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        )
    }

    /// Wait for the drain thread to finish, after the child has exited.
    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            // A panicked drain thread costs us the stderr tail and nothing
            // else; the child's exit status is still authoritative.
            let _ = handle.join();
        }
    }
}

/// One progress report parsed from ffmpeg's `-progress` output.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// Output position reached, in seconds.
    pub out_time_seconds: f64,
    /// Frames written so far.
    pub frame: u64,
    /// Encoding rate relative to real time, when ffmpeg reports it.
    pub speed: Option<f64>,
    /// Frames per second, when ffmpeg reports it.
    pub fps: Option<f64>,
}

/// Run an ffmpeg command to completion, reporting progress as it goes.
///
/// The command must already be configured to write `-progress` output to
/// stdout; [`crate::encode::progress_args`] supplies the flags for that.
///
/// # Errors
/// Returns [`Error::Spawn`] if the process cannot start, or [`Error::Failed`]
/// with the captured stderr tail if it exits non-zero.
pub fn run_with_progress(
    mut command: Command,
    mut on_progress: impl FnMut(Progress),
) -> Result<(), Error> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let program = command.get_program().display().to_string();
    let mut child = command.spawn().map_err(|source| Error::Spawn {
        program: program.clone(),
        source,
    })?;

    let mut stderr = StderrTail::spawn(&mut child);

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut current = Progress::default();
        for line in reader.lines().map_while(Result::ok) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "out_time_us" | "out_time_ms" => {
                    // Despite the name, ffmpeg reports both of these in
                    // microseconds; `out_time_ms` is a long-standing misnomer.
                    if let Ok(micros) = value.parse::<i64>() {
                        current.out_time_seconds = micros as f64 / 1_000_000.0;
                    }
                }
                "frame" => {
                    if let Ok(frame) = value.parse() {
                        current.frame = frame;
                    }
                }
                "fps" => current.fps = value.parse().ok(),
                "speed" => current.speed = value.trim_end_matches('x').parse().ok(),
                // `progress` terminates each block, so this is where a
                // complete, self-consistent snapshot is ready to report.
                "progress" => on_progress(current),
                _ => {}
            }
        }
    }

    let status = child.wait().map_err(|source| Error::Spawn {
        program: program.clone(),
        source,
    })?;
    stderr.join();

    if status.success() {
        Ok(())
    } else {
        Err(Error::Failed {
            program,
            code: status.code(),
            stderr: stderr.text(),
        })
    }
}

/// Read a child's stdout to the end, returning it as bytes.
///
/// Used for the short, bounded outputs of `ffprobe`.
///
/// # Errors
/// Returns [`Error::Failed`] when the process exits non-zero.
pub fn capture_stdout(mut command: Command) -> Result<Vec<u8>, Error> {
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let program = command.get_program().display().to_string();
    let mut child = command.spawn().map_err(|source| Error::Spawn {
        program: program.clone(),
        source,
    })?;

    let mut stderr = StderrTail::spawn(&mut child);

    let mut output = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout
            .read_to_end(&mut output)
            .map_err(|source| Error::Spawn {
                program: program.clone(),
                source,
            })?;
    }

    let status = child.wait().map_err(|source| Error::Spawn {
        program: program.clone(),
        source,
    })?;
    stderr.join();

    if status.success() {
        Ok(output)
    } else {
        Err(Error::Failed {
            program,
            code: status.code(),
            stderr: stderr.text(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_of_a_successful_command() {
        let mut command = Command::new("echo");
        command.arg("hello");
        let output = capture_stdout(command).expect("echo succeeds");
        assert_eq!(String::from_utf8_lossy(&output).trim(), "hello");
    }

    #[test]
    fn reports_the_stderr_tail_when_a_command_fails() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo 'the real cause' >&2; exit 3"]);
        let error = capture_stdout(command).expect_err("exit 3 is a failure");
        match error {
            Error::Failed { code, stderr, .. } => {
                assert_eq!(code, Some(3));
                assert!(stderr.contains("the real cause"), "stderr was {stderr:?}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn reports_a_missing_binary_as_a_spawn_error() {
        let command = Command::new("asaborake-no-such-binary");
        assert!(matches!(capture_stdout(command), Err(Error::Spawn { .. })));
    }

    #[test]
    fn parses_progress_blocks_into_snapshots() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'frame=10\\nfps=25.0\\nout_time_us=400000\\nspeed=1.5x\\nprogress=continue\\n\
             frame=20\\nout_time_us=800000\\nprogress=end\\n'",
        ]);

        let mut seen = Vec::new();
        run_with_progress(command, |p| seen.push(p)).expect("command succeeds");

        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].frame, 10);
        assert!((seen[0].out_time_seconds - 0.4).abs() < 1e-9);
        assert_eq!(seen[0].speed, Some(1.5));
        assert_eq!(seen[0].fps, Some(25.0));
        assert_eq!(seen[1].frame, 20);
        assert!((seen[1].out_time_seconds - 0.8).abs() < 1e-9);
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "for i in $(seq 1 200); do echo line$i >&2; done; exit 1",
        ]);
        let error = capture_stdout(command).expect_err("exit 1 is a failure");
        let Error::Failed { stderr, .. } = error else {
            panic!("expected Failed");
        };
        let lines = stderr.lines().count();
        assert!(lines <= STDERR_TAIL_LINES, "kept {lines} lines");
        assert!(stderr.contains("line200"), "the tail must include the end");
        assert!(!stderr.contains("line1\n"), "the head should be dropped");
    }

    #[test]
    fn a_repeated_complaint_does_not_crowd_out_the_real_cause() {
        // What ffmpeg does on a damaged recording: cycle a couple of decoder
        // complaints once per frame, then say what actually killed it. Without
        // folding the repeats, the last line is all that survives.
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "for i in $(seq 1 300); do \
               echo 'Error submitting packet to decoder' >&2; \
               echo 'Invalid frame dimensions 0x0.' >&2; \
             done; \
             echo 'Decode error rate 1 exceeds maximum 0.666667' >&2; \
             exit 1",
        ]);
        let error = capture_stdout(command).expect_err("exit 1 is a failure");
        let Error::Failed { stderr, .. } = error else {
            panic!("expected Failed");
        };

        assert!(
            stderr.contains("Decode error rate 1 exceeds maximum"),
            "the cause must survive: {stderr}"
        );
        assert!(
            stderr.contains("(×300)"),
            "the repeats must be counted rather than listed: {stderr}"
        );
        assert!(
            stderr.lines().count() <= 3,
            "six hundred lines should fold to three: {stderr}"
        );
    }
}
