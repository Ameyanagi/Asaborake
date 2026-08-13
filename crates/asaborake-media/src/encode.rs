//! Shared pieces of an encode invocation.
//!
//! Profile-specific argument building lives in `asaborake-core`, which knows
//! about codecs and quality settings. What lives here is the machinery every
//! encode needs regardless of profile: progress reporting and chapter
//! metadata.

use std::fmt::Write as _;

/// Arguments that make ffmpeg report progress on stdout in a parseable form.
///
/// `-nostats` suppresses the human-readable status line, which would otherwise
/// interleave with the key/value output and has to be filtered back out.
#[must_use]
pub fn progress_args() -> [&'static str; 3] {
    ["-progress", "pipe:1", "-nostats"]
}

/// One chapter in the output file.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    /// Start of the chapter, in seconds from the start of the output.
    pub start_seconds: f64,
    /// End of the chapter, in seconds from the start of the output.
    pub end_seconds: f64,
    /// Title shown by players.
    pub title: String,
}

/// Render chapters as an ffmetadata document.
///
/// Chapters are how a viewer navigates a recording that kept its commercials,
/// and how anyone reviews the cuts on one that did not — so Asaborake writes
/// them either way.
#[must_use]
pub fn ffmetadata(chapters: &[Chapter]) -> String {
    // A millisecond timebase is finer than any chapter needs and avoids the
    // rounding surprises of ffmpeg's default.
    let mut out = String::from(";FFMETADATA1\n");
    for chapter in chapters {
        let start = (chapter.start_seconds.max(0.0) * 1000.0).round() as i64;
        let end = (chapter.end_seconds.max(0.0) * 1000.0).round() as i64;
        out.push_str("\n[CHAPTER]\nTIMEBASE=1/1000\n");
        let _ = writeln!(out, "START={start}");
        let _ = writeln!(out, "END={}", end.max(start));
        let _ = writeln!(out, "title={}", escape_metadata(&chapter.title));
    }
    out
}

/// Escape the characters ffmetadata treats as syntax.
fn escape_metadata(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '=' | ';' | '#' | '\\' | '\n') {
            out.push('\\');
        }
        // A literal newline inside a value would end the entry, so it is
        // replaced rather than merely escaped.
        out.push(if character == '\n' { ' ' } else { character });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_chapters_with_a_millisecond_timebase() {
        let chapters = [
            Chapter {
                start_seconds: 0.0,
                end_seconds: 90.5,
                title: "Programme".into(),
            },
            Chapter {
                start_seconds: 90.5,
                end_seconds: 150.0,
                title: "CM".into(),
            },
        ];
        let text = ffmetadata(&chapters);
        assert!(text.starts_with(";FFMETADATA1\n"));
        assert!(text.contains("TIMEBASE=1/1000"));
        assert!(text.contains("START=0\nEND=90500\ntitle=Programme"));
        assert!(text.contains("START=90500\nEND=150000\ntitle=CM"));
    }

    #[test]
    fn escapes_metadata_syntax_in_titles() {
        let chapters = [Chapter {
            start_seconds: 0.0,
            end_seconds: 1.0,
            // A programme title really can contain these.
            title: "News #1 = live; part\\2".into(),
        }];
        let text = ffmetadata(&chapters);
        assert!(
            text.contains("title=News \\#1 \\= live\\; part\\\\2"),
            "got: {text}"
        );
    }

    #[test]
    fn replaces_newlines_rather_than_ending_the_entry() {
        let chapters = [Chapter {
            start_seconds: 0.0,
            end_seconds: 1.0,
            title: "two\nlines".into(),
        }];
        let text = ffmetadata(&chapters);
        assert!(text.contains("title=two\\ lines"), "got: {text}");
        assert_eq!(text.matches("[CHAPTER]").count(), 1);
    }

    #[test]
    fn a_zero_length_chapter_does_not_end_before_it_starts() {
        let chapters = [Chapter {
            start_seconds: 10.0,
            end_seconds: 5.0,
            title: "degenerate".into(),
        }];
        assert!(ffmetadata(&chapters).contains("START=10000\nEND=10000"));
    }

    #[test]
    fn progress_args_request_machine_readable_output() {
        assert_eq!(progress_args(), ["-progress", "pipe:1", "-nostats"]);
    }
}
