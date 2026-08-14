//! Naming the output after the programme rather than after the file.
//!
//! `EPGStation` hands over a path it chose, and Asaborake used it verbatim. So
//! a year of recordings comes out as a flat directory of whatever the recorder
//! called them, when everything needed to file them properly — the programme's
//! title, its channel, when it was on — is already in hand.
//!
//! Amatsukaze renames from the programme metadata and can sort into folders by
//! genre. Genre needs ARIB genre parsing, which `asaborake-ts` does not do yet;
//! everything else is available today.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, Timelike};

/// What a template may refer to.
#[derive(Debug, Clone, Default)]
pub struct Fields {
    /// Programme title.
    pub title: Option<String>,
    /// Channel name.
    pub channel: Option<String>,
    /// When the recording started.
    pub recorded_at: Option<DateTime<Local>>,
    /// The source file's own stem, as a fallback.
    pub source: Option<String>,
}

impl Fields {
    /// The value of one placeholder, if it has one.
    fn get(&self, key: &str) -> Option<String> {
        match key {
            "title" => self.title.clone(),
            "channel" => self.channel.clone(),
            "source" => self.source.clone(),
            "date" => self
                .recorded_at
                .map(|at| format!("{:04}-{:02}-{:02}", at.year(), at.month(), at.day())),
            "time" => self
                .recorded_at
                .map(|at| format!("{:02}{:02}", at.hour(), at.minute())),
            "year" => self.recorded_at.map(|at| format!("{:04}", at.year())),
            "month" => self.recorded_at.map(|at| format!("{:02}", at.month())),
            _ => None,
        }
    }
}

/// Characters a path component may not contain.
///
/// Slash and the Windows-reserved set, because recordings routinely end up on
/// a share that a Windows machine reads. A colon in "Programme: The Sequel" is
/// the common case and would otherwise produce a name that copies badly.
const FORBIDDEN: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];

/// Make one path component safe to write.
///
/// Deliberately not a general sanitiser: it is applied to a *substituted
/// value*, never to the template, so a template may still contain slashes and
/// build a directory tree while a programme title containing one cannot.
#[must_use]
pub fn sanitise(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if FORBIDDEN.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    // A leading dot hides the file; trailing dots and spaces are silently
    // dropped by Windows, which turns two distinct names into one.
    let trimmed = cleaned.trim().trim_end_matches('.').trim_start_matches('.');
    if trimmed.is_empty() {
        "untitled".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Expand `{placeholders}` in a template.
///
/// A placeholder with nothing behind it expands to nothing, and the surrounding
/// separators are tidied afterwards, so a template written for a recording that
/// has a channel still reads properly for one that does not.
#[must_use]
pub fn expand(template: &str, fields: &Fields) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            // An unclosed brace is a typo in a configuration file. Emitting it
            // verbatim makes the mistake visible in the file name, which is
            // where somebody will notice it.
            break;
        };
        let key = &rest[open + 1..open + close];
        if let Some(value) = fields.get(key) {
            out.push_str(&sanitise(&value));
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);

    tidy(&out)
}

/// Collapse the separators left behind by an empty placeholder.
fn tidy(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut previous_separator = false;
    for c in value.chars() {
        let separator = matches!(c, ' ' | '-' | '_');
        if separator && previous_separator {
            continue;
        }
        previous_separator = separator;
        out.push(c);
    }
    out.trim().trim_matches('-').trim().to_owned()
}

/// Work out where a job's output should go.
///
/// `template` names the file, not the path: the directory comes from the
/// output the caller asked for, so a deployment that has already decided where
/// recordings live keeps that decision. A template containing slashes builds
/// subdirectories beneath it.
///
/// Returns `None` when the template produces nothing usable, in which case the
/// caller's own path stands.
#[must_use]
pub fn rename(output: &Path, template: &str, fields: &Fields) -> Option<PathBuf> {
    let expanded = expand(template, fields);
    if expanded.trim().is_empty() {
        return None;
    }

    let extension = output
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let directory = output.parent().unwrap_or(Path::new("."));
    Some(directory.join(format!("{expanded}{extension}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn fields() -> Fields {
        Fields {
            title: Some("ぐるナイ 2時間SP".to_owned()),
            channel: Some("日本テレビ".to_owned()),
            recorded_at: Local.with_ymd_and_hms(2026, 8, 13, 20, 5, 0).single(),
            source: Some("recording-1234".to_owned()),
        }
    }

    #[test]
    fn a_template_becomes_the_programme_it_names() {
        assert_eq!(
            expand("{date} {channel} {title}", &fields()),
            "2026-08-13 日本テレビ ぐるナイ 2時間SP"
        );
    }

    #[test]
    fn a_placeholder_with_nothing_behind_it_leaves_no_trace() {
        // A template written for recordings that have a channel must still
        // read properly for one that does not.
        let sparse = Fields {
            title: Some("News".to_owned()),
            ..Fields::default()
        };
        assert_eq!(expand("{date} - {channel} - {title}", &sparse), "News");
        assert_eq!(expand("{channel} {title}", &sparse), "News");
    }

    #[test]
    fn a_title_cannot_escape_its_directory() {
        // The value is sanitised, the template is not — which is what lets a
        // template build a tree while a programme title cannot.
        let nasty = Fields {
            title: Some("../../etc/passwd".to_owned()),
            ..Fields::default()
        };
        let expanded = expand("{title}", &nasty);
        assert!(!expanded.contains('/'), "{expanded}");

        // But the template's own slashes still make directories.
        assert_eq!(
            expand("{channel}/{title}", &fields()),
            "日本テレビ/ぐるナイ 2時間SP"
        );
    }

    #[test]
    fn characters_that_travel_badly_are_replaced() {
        // Recordings end up on a share a Windows machine reads, and a colon in
        // "Programme: The Sequel" is the common case.
        assert_eq!(sanitise("Drama: Part 2"), "Drama_ Part 2");
        assert_eq!(sanitise("a?b*c|d"), "a_b_c_d");
        assert_eq!(sanitise("  .hidden.  "), "hidden");
        assert_eq!(sanitise("   "), "untitled");
    }

    #[test]
    fn renaming_keeps_the_directory_and_the_extension() {
        let output = Path::new("/recordings/whatever-epgstation-called-it.mp4");
        let renamed = rename(output, "{date} {title}", &fields()).expect("a name");

        assert_eq!(
            renamed,
            PathBuf::from("/recordings/2026-08-13 ぐるナイ 2時間SP.mp4")
        );
    }

    #[test]
    fn a_template_that_expands_to_nothing_leaves_the_path_alone() {
        // Better to write where the caller asked than to invent a name.
        let output = Path::new("/recordings/show.mp4");
        assert_eq!(rename(output, "{title}", &Fields::default()), None);
    }

    #[test]
    fn a_template_may_build_subdirectories() {
        let output = Path::new("/recordings/x.mp4");
        let renamed = rename(output, "{year}/{channel}/{title}", &fields()).expect("a name");
        assert_eq!(
            renamed,
            PathBuf::from("/recordings/2026/日本テレビ/ぐるナイ 2時間SP.mp4")
        );
    }

    #[test]
    fn an_unclosed_brace_is_left_where_it_can_be_seen() {
        // A typo in a configuration file, and the file name is where somebody
        // will notice it.
        let expanded = expand("{title} {broken", &fields());
        assert!(expanded.contains('{'), "{expanded}");
    }
}
