//! End-to-end checks against a real ffmpeg.
//!
//! These generate their own material rather than shipping a recording, so the
//! suite stays runnable anywhere and carries no broadcast content. They skip
//! rather than fail when ffmpeg is absent, because a contributor working on
//! the web UI should not need it installed.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use asaborake_media::{Ffmpeg, FrameReader, FrameReaderOptions, probe, rms_envelope};

/// Locate ffmpeg, or return `None` so the caller can skip.
fn ffmpeg() -> Option<Ffmpeg> {
    match Ffmpeg::discover(None, None) {
        Ok(found) => Some(found),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

/// Render a short clip: a moving test pattern with a tone that goes silent
/// for the middle second, which is the shape every detector here looks for.
fn render_clip(path: &Path, seconds: u32, width: u32, height: u32, fps: u32) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={width}x{height}:rate={fps}:duration={seconds}"),
        ])
        // A tone for the first and last third, silence through the middle.
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!(
                "aevalsrc=0.5*sin(1000*2*PI*t)*between(t\\,0\\,1)+0.5*sin(1000*2*PI*t)*between(t\\,2\\,{seconds}):s=48000:d={seconds}"
            ),
        ])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "failed to render the test clip");
}

#[test]
fn probes_geometry_frame_rate_and_audio() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let clip = dir.path().join("clip.mp4");
    render_clip(&clip, 3, 320, 240, 25);

    let probed = probe(&ffmpeg, &clip).expect("probe succeeds");

    let video = probed.video.expect("a video stream");
    assert_eq!((video.width, video.height), (320, 240));
    assert!((video.fps() - 25.0).abs() < 0.01, "fps was {}", video.fps());

    assert_eq!(probed.audio.len(), 1);
    assert_eq!(probed.audio[0].sample_rate, 48_000);

    let duration = probed.duration_seconds.expect("a duration");
    assert!((duration - 3.0).abs() < 0.2, "duration was {duration}");
}

#[test]
fn reads_every_frame_with_exact_timestamps() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let clip = dir.path().join("clip.mp4");
    render_clip(&clip, 2, 160, 120, 25);

    let probed = probe(&ffmpeg, &clip).expect("probe succeeds");
    let mut reader = FrameReader::open(&ffmpeg, &clip, &probed, &FrameReaderOptions::default())
        .expect("reader opens");

    assert_eq!((reader.width(), reader.height()), (160, 120));

    let mut count = 0u64;
    let mut last_timestamp = -1.0;
    while let Some(frame) = reader.next_frame().expect("frames decode") {
        assert_eq!(frame.luma.len(), 160 * 120, "gray8 is one byte per pixel");
        assert!(
            frame.timestamp > last_timestamp,
            "timestamps must advance: {} after {last_timestamp}",
            frame.timestamp
        );
        last_timestamp = frame.timestamp;
        count += 1;
    }

    // 2 seconds at 25 fps, allowing for the encoder trimming an edge frame.
    assert!((49..=51).contains(&count), "decoded {count} frames");
    assert!(
        (last_timestamp - 1.96).abs() < 0.1,
        "last timestamp was {last_timestamp}"
    );
}

#[test]
fn decimation_returns_every_nth_frame_at_the_right_times() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let clip = dir.path().join("clip.mp4");
    render_clip(&clip, 2, 160, 120, 25);

    let probed = probe(&ffmpeg, &clip).expect("probe succeeds");
    let options = FrameReaderOptions {
        select_every: 5,
        ..FrameReaderOptions::default()
    };
    let mut reader = FrameReader::open(&ffmpeg, &clip, &probed, &options).expect("reader opens");

    // Five frames at 25 fps is a fifth of a second per returned frame.
    assert!(
        (reader.seconds_per_frame() - 0.2).abs() < 1e-6,
        "step was {}",
        reader.seconds_per_frame()
    );

    let mut count = 0u64;
    while reader.next_frame().expect("frames decode").is_some() {
        count += 1;
    }
    assert!((9..=11).contains(&count), "decoded {count} frames");
}

#[test]
fn scaling_happens_in_ffmpeg_when_requested() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let clip = dir.path().join("clip.mp4");
    render_clip(&clip, 1, 320, 240, 25);

    let probed = probe(&ffmpeg, &clip).expect("probe succeeds");
    let options = FrameReaderOptions {
        scale: Some((80, 60)),
        ..FrameReaderOptions::default()
    };
    let mut reader = FrameReader::open(&ffmpeg, &clip, &probed, &options).expect("reader opens");

    let frame = reader
        .next_frame()
        .expect("frames decode")
        .expect("at least one frame");
    assert_eq!((frame.width, frame.height), (80, 60));
    assert_eq!(frame.luma.len(), 80 * 60);
}

#[test]
fn finds_the_silent_stretch_in_the_middle_of_the_clip() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let clip = dir.path().join("clip.mp4");
    render_clip(&clip, 3, 160, 120, 25);

    let envelope = rms_envelope(&ffmpeg, &clip, 0.02).expect("envelope computes");
    assert!(
        (envelope.duration_seconds() - 3.0).abs() < 0.2,
        "envelope covered {}s",
        envelope.duration_seconds()
    );

    let spans = envelope.silent_spans(-50.0, 0.3);
    assert_eq!(spans.len(), 1, "expected one silent span, got {spans:?}");

    let (start, end) = spans[0];
    // The tone stops at 1s and resumes at 2s; AAC's encoder delay smears the
    // edges by a few tens of milliseconds.
    assert!((start - 1.0).abs() < 0.15, "silence started at {start}");
    assert!((end - 2.0).abs() < 0.15, "silence ended at {end}");
}

#[test]
fn a_missing_input_fails_with_the_ffmpeg_error_text() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let error = probe(&ffmpeg, Path::new("/nonexistent/asaborake.ts")).expect_err("must fail");
    let message = error.to_string();
    assert!(
        message.contains("exited with") || message.contains("No such file"),
        "unhelpful error: {message}"
    );
}
