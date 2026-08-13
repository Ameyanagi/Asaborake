//! End-to-end encode checks against a real ffmpeg.
//!
//! The filter chain that applies the cuts is assembled as a string and handed
//! to ffmpeg, so a mistake in escaping it fails at runtime rather than at
//! compile time — and fails *quietly*, by producing a file of the wrong length
//! rather than an error. These tests cut a generated clip and measure what
//! came out.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use asaborake_cmcut::KeepRange;
use asaborake_core::encode::{EncodeRequest, encode};
use asaborake_core::profile::{Profile, builtin};
use asaborake_media::{Chapter, Ffmpeg, probe};

fn ffmpeg() -> Option<Ffmpeg> {
    match Ffmpeg::discover(None, None) {
        Ok(found) => Some(found),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

/// The software profile, so these run without a GPU.
fn cpu_profile() -> Profile {
    let mut profile = builtin().remove("x264-cpu").expect("the cpu profile");
    // Keep the tests quick; quality is not what is under test.
    profile.video.args = vec![
        "-preset".into(),
        "ultrafast".into(),
        "-crf".into(),
        "30".into(),
    ];
    profile.video.max_height = None;
    profile
}

/// Render a clip of `seconds` at 25 fps with a continuous tone.
fn render_clip(path: &Path, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=160x120:rate=25:duration={seconds}"),
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={seconds}"),
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "failed to render the test clip");
}

#[test]
fn cutting_removes_exactly_the_ranges_that_were_not_kept() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");
    render_clip(&input, 12);

    let source = probe(&ffmpeg, &input).expect("probe");
    // Keep 0-3s and 6-9s: six seconds of a twelve-second clip.
    let keep = [
        KeepRange {
            start: 0.0,
            end: 3.0,
        },
        KeepRange {
            start: 6.0,
            end: 9.0,
        },
    ];
    let profile = cpu_profile();

    let mut seen_progress = Vec::new();
    encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &keep,
            chapters: &[],
            probe: &source,
            dual_mono: None,
        },
        &mut |fraction| seen_progress.push(fraction),
    )
    .expect("encode succeeds");

    let result = probe(&ffmpeg, &output).expect("probe output");
    let duration = result.duration_seconds.expect("a duration");
    assert!(
        (duration - 6.0).abs() < 0.35,
        "expected about 6s of output, got {duration}s"
    );

    assert!(!seen_progress.is_empty(), "progress must be reported");
    assert!(
        seen_progress.iter().all(|f| (0.0..=1.0).contains(f)),
        "progress must stay in range: {seen_progress:?}"
    );
}

#[test]
fn a_single_full_length_range_passes_the_clip_through_unchanged() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");
    render_clip(&input, 6);

    let source = probe(&ffmpeg, &input).expect("probe");
    let keep = [KeepRange {
        start: 0.0,
        end: source.duration_seconds.expect("a duration"),
    }];
    let profile = cpu_profile();

    encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &keep,
            chapters: &[],
            probe: &source,
            dual_mono: None,
        },
        &mut |_| {},
    )
    .expect("encode succeeds");

    let result = probe(&ffmpeg, &output).expect("probe output");
    let duration = result.duration_seconds.expect("a duration");
    assert!(
        (duration - 6.0).abs() < 0.35,
        "expected the whole clip, got {duration}s"
    );
}

#[test]
fn chapters_reach_the_output_file() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");
    render_clip(&input, 8);

    let source = probe(&ffmpeg, &input).expect("probe");
    let keep = [KeepRange {
        start: 0.0,
        end: 8.0,
    }];
    let chapters = [
        Chapter {
            start_seconds: 0.0,
            end_seconds: 4.0,
            title: "Part 1".into(),
        },
        Chapter {
            start_seconds: 4.0,
            end_seconds: 8.0,
            title: "CM 1".into(),
        },
    ];
    let profile = cpu_profile();

    encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &keep,
            chapters: &chapters,
            probe: &source,
            dual_mono: None,
        },
        &mut |_| {},
    )
    .expect("encode succeeds");

    let listed = Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-print_format",
            "json",
            "-show_chapters",
        ])
        .arg(&output)
        .output()
        .expect("ffprobe runs");
    let text = String::from_utf8_lossy(&listed.stdout);

    assert!(text.contains("Part 1"), "chapters missing from {text}");
    assert!(text.contains("CM 1"), "chapters missing from {text}");
}

/// Render a clip with two distinct audio tracks, as a bilingual programme has.
fn render_bilingual(path: &Path, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=160x120:rate=25:duration={seconds}"),
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={seconds}"),
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=880:sample_rate=48000:duration={seconds}"),
        ])
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-c:a", "aac", "-shortest"])
        .args(["-metadata:s:a:0", "language=jpn"])
        .args(["-metadata:s:a:1", "language=eng"])
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "failed to render the bilingual clip");
}

/// Count the audio streams in a file.
fn audio_tracks(path: &Path) -> usize {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

#[test]
fn both_audio_tracks_survive_a_cut() {
    // A bilingual programme carries two audio streams. Mapping only the first
    // silently discards a language, which is what Asaborake used to do.
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");
    render_bilingual(&input, 12);

    let source = probe(&ffmpeg, &input).expect("probe");
    assert_eq!(source.audio.len(), 2, "the fixture should have two tracks");

    let keep = [
        KeepRange {
            start: 0.0,
            end: 3.0,
        },
        KeepRange {
            start: 6.0,
            end: 9.0,
        },
    ];
    let profile = cpu_profile();

    encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &keep,
            chapters: &[],
            probe: &source,
            dual_mono: None,
        },
        &mut |_| {},
    )
    .expect("encode succeeds");

    assert_eq!(audio_tracks(&output), 2, "both languages must survive");

    let result = probe(&ffmpeg, &output).expect("probe output");
    let duration = result.duration_seconds.expect("a duration");
    assert!(
        (duration - 6.0).abs() < 0.35,
        "expected about 6s, got {duration}s"
    );
}

#[test]
fn audio_is_copied_when_nothing_is_cut() {
    // Broadcast audio is already AAC; re-encoding it only loses quality. The
    // give-away is that a copied AAC stream keeps its exact codec and rate.
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");
    render_bilingual(&input, 6);

    let source = probe(&ffmpeg, &input).expect("probe");
    let keep = [KeepRange {
        start: 0.0,
        end: source.duration_seconds.expect("a duration"),
    }];
    let profile = cpu_profile();

    encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &keep,
            chapters: &[],
            probe: &source,
            dual_mono: None,
        },
        &mut |_| {},
    )
    .expect("encode succeeds");

    let result = probe(&ffmpeg, &output).expect("probe output");
    assert_eq!(result.audio.len(), 2, "both tracks copied");
    assert!(
        result.audio.iter().all(|a| a.codec == "aac"),
        "copied streams stay AAC: {:?}",
        result.audio
    );
}

/// Render a clip with one stereo audio stream carrying a different tone in
/// each channel, as an ARIB bilingual programme carries a language per channel.
fn render_dual_mono(path: &Path, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size=160x120:rate=25:duration={seconds}"),
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={seconds}"),
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=880:sample_rate=48000:duration={seconds}"),
        ])
        // One stream, two channels, a different programme on each.
        .args(["-filter_complex", "[1:a][2:a]amerge=inputs=2[a]"])
        .args(["-map", "0:v", "-map", "[a]"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "failed to render the dual-mono clip");
}

#[test]
fn a_bilingual_programme_comes_out_as_two_single_language_tracks() {
    // The filter graph that splits the channels is a string handed to ffmpeg,
    // so a mistake in it fails at runtime rather than at compile time. This
    // runs it.
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");
    render_dual_mono(&input, 6);

    let source = probe(&ffmpeg, &input).expect("probe");
    assert_eq!(source.audio.len(), 1, "the fixture is one stream");
    assert_eq!(source.audio[0].channels, 2, "carrying two channels");

    let keep = [KeepRange {
        start: 0.0,
        end: source.duration_seconds.expect("a duration"),
    }];
    let dual = asaborake_core::diagnostics::DualMono {
        main: Some("jpn".into()),
        sub: Some("eng".into()),
    };
    let profile = cpu_profile();

    encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &keep,
            chapters: &[],
            probe: &source,
            dual_mono: Some(&dual),
        },
        &mut |_| {},
    )
    .expect("encode succeeds");

    let result = probe(&ffmpeg, &output).expect("probe output");
    assert_eq!(result.audio.len(), 2, "one track per language");
    assert!(
        result.audio.iter().all(|a| a.channels == 1),
        "each language is mono, not a duplicated stereo pair: {:?}",
        result.audio
    );

    let languages: Vec<Option<&str>> = result.audio.iter().map(|a| a.language.as_deref()).collect();
    assert_eq!(
        languages,
        vec![Some("jpn"), Some("eng")],
        "the tags are the only thing telling the two apart"
    );
}

#[test]
fn a_profile_the_build_cannot_run_fails_before_doing_any_work() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.mp4");
    let output = dir.path().join("out.mp4");

    let mut profile = cpu_profile();
    profile.video.encoder = "definitely_not_an_encoder".into();

    let source = asaborake_media::MediaProbe {
        duration_seconds: Some(10.0),
        video: None,
        audio: Vec::new(),
    };
    let error = encode(
        &ffmpeg,
        &EncodeRequest {
            input: &input,
            output: &output,
            profile: &profile,
            keep: &[],
            chapters: &[],
            probe: &source,
            dual_mono: None,
        },
        &mut |_| {},
    )
    .expect_err("an unavailable encoder must fail");

    assert!(
        error.to_string().contains("definitely_not_an_encoder"),
        "unhelpful error: {error}"
    );
    assert!(!output.exists(), "nothing should have been written");
}
