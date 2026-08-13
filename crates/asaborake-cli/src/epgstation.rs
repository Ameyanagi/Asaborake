//! The `EPGStation` encoder contract.
//!
//! `EPGStation` runs an external encoder as a plain child process. It does not
//! hand over a config file or command-line arguments beyond what the operator
//! wrote in `config.yml`; everything about the recording arrives as
//! environment variables, and everything Asaborake wants to report goes back
//! as newline-delimited JSON on stdout:
//!
//! ```text
//! {"type":"progress","percent":0.42,"log":"encoding"}
//! ```
//!
//! `percent` is a fraction, not a percentage — `EPGStation`'s client multiplies
//! by 100 before display. Exit code 0 means success; on anything else
//! `EPGStation` deletes the output file, which is the right behaviour and means
//! a failed job must not exit zero after writing a partial file.
//!
//! Verified against `EPGStation`'s `EncoderModel.ts` and `ProcessUtil.ts`.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

/// What `EPGStation` told us about the recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Environment {
    /// Source recording. `EPGStation` substitutes this for `%INPUT%` too.
    pub(crate) input: Option<PathBuf>,
    /// Where the result must be written.
    pub(crate) output: Option<PathBuf>,
    /// Programme name.
    pub(crate) name: Option<String>,
    /// Channel id, which keys the logo store.
    pub(crate) channel_id: Option<String>,
    /// Channel name, used to label a newly learned logo.
    pub(crate) channel_name: Option<String>,
    /// Source height as `EPGStation` recorded it, e.g. `1080`.
    pub(crate) video_resolution: Option<String>,
    /// `EPGStation`'s own recorded-file id, for correlating logs.
    pub(crate) recorded_id: Option<String>,
}

impl Environment {
    /// Read the variables `EPGStation` sets.
    #[must_use]
    pub(crate) fn from_env() -> Self {
        Self::from_pairs(&std::env::vars().collect::<Vec<_>>())
    }

    /// Build from explicit pairs, which is what makes this testable.
    #[must_use]
    pub(crate) fn from_pairs(pairs: &[(String, String)]) -> Self {
        let get = |key: &str| -> Option<String> {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };

        Self {
            input: get("INPUT").map(PathBuf::from),
            output: get("OUTPUT").map(PathBuf::from),
            // `EPGStation` supplies both a full-width and a half-width form.
            // The half-width one is the better filename and the better log
            // line, so it wins when present.
            name: get("HALF_WIDTH_NAME").or_else(|| get("NAME")),
            channel_id: get("CHANNELID"),
            channel_name: get("HALF_WIDTH_CHANNELNAME").or_else(|| get("CHANNELNAME")),
            video_resolution: get("VIDEORESOLUTION"),
            recorded_id: get("RECORDEDID"),
        }
    }
}

/// One progress line, in the shape `EPGStation` parses.
#[derive(Debug, Clone, Serialize)]
struct ProgressLine<'a> {
    /// Always `progress`; `EPGStation` ignores anything else.
    #[serde(rename = "type")]
    kind: &'static str,
    /// Completion as a fraction of one.
    percent: f64,
    /// A short status, shown beside the bar.
    log: &'a str,
}

/// Writes progress in `EPGStation`'s format, at a rate it can keep up with.
#[derive(Debug)]
pub(crate) struct ProgressReporter {
    last_reported: f64,
    last_message: String,
}

impl Default for ProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter {
    /// Create a reporter that has not yet written anything.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            // Below zero so the first report always goes out.
            last_reported: -1.0,
            last_message: String::new(),
        }
    }

    /// Whether a report is worth writing.
    ///
    /// `EPGStation` parses every line and emits a UI event per progress update,
    /// so reporting each of a hundred thousand frames would cost more than the
    /// encode. A report goes out when the bar has visibly moved or the message
    /// has changed.
    #[must_use]
    pub(crate) fn should_report(&self, fraction: f64, message: &str) -> bool {
        message != self.last_message || fraction - self.last_reported >= 0.005
    }

    /// Write a progress line, if it is worth writing.
    ///
    /// Failures to write are ignored: a broken stdout means `EPGStation` has
    /// gone away, and the encode should run to completion regardless rather
    /// than dying halfway and leaving a partial file.
    pub(crate) fn report(&mut self, out: &mut impl Write, fraction: f64, message: &str) {
        if !self.should_report(fraction, message) {
            return;
        }
        self.last_reported = fraction;
        message.clone_into(&mut self.last_message);

        let line = ProgressLine {
            kind: "progress",
            percent: fraction.clamp(0.0, 1.0),
            log: message,
        };
        if let Ok(json) = serde_json::to_string(&line) {
            let _ = writeln!(out, "{json}");
            let _ = out.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn reads_the_variables_epgstation_sets() {
        let env = Environment::from_pairs(&pairs(&[
            ("INPUT", "/recordings/news.ts"),
            ("OUTPUT", "/recordings/news.mp4"),
            ("NAME", "ニュース"),
            ("CHANNELID", "3239123"),
            ("CHANNELNAME", "ＮＨＫ総合"),
            ("HALF_WIDTH_CHANNELNAME", "NHK総合"),
            ("VIDEORESOLUTION", "1080i"),
            ("RECORDEDID", "42"),
        ]));

        assert_eq!(env.input, Some(PathBuf::from("/recordings/news.ts")));
        assert_eq!(env.output, Some(PathBuf::from("/recordings/news.mp4")));
        assert_eq!(env.channel_id.as_deref(), Some("3239123"));
        // The half-width channel name is the more useful of the pair.
        assert_eq!(env.channel_name.as_deref(), Some("NHK総合"));
        assert_eq!(env.recorded_id.as_deref(), Some("42"));
    }

    #[test]
    fn empty_variables_are_treated_as_absent() {
        // `EPGStation` sets every variable, using an empty string for the ones
        // it has no value for, so presence alone means nothing.
        let env = Environment::from_pairs(&pairs(&[
            ("INPUT", "/in.ts"),
            ("OUTPUT", ""),
            ("CHANNELID", ""),
        ]));
        assert!(env.output.is_none());
        assert!(env.channel_id.is_none());
    }

    #[test]
    fn progress_lines_match_the_shape_epgstation_parses() {
        let mut out = Vec::new();
        let mut reporter = ProgressReporter::new();
        reporter.report(&mut out, 0.42, "encoding");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.ends_with('\n'), "must be newline delimited: {text:?}");

        let parsed: serde_json::Value = serde_json::from_str(text.trim()).expect("valid json");
        assert_eq!(parsed["type"], "progress");
        assert_eq!(parsed["log"], "encoding");
        // A fraction, not a percentage: the client multiplies by 100.
        assert!((parsed["percent"].as_f64().expect("a number") - 0.42).abs() < 1e-9);
    }

    #[test]
    fn progress_is_clamped_into_range() {
        let mut out = Vec::new();
        let mut reporter = ProgressReporter::new();
        reporter.report(&mut out, 1.5, "over");
        reporter.report(&mut out, -0.5, "under");

        let text = String::from_utf8(out).expect("utf-8");
        for line in text.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("valid json");
            let percent = parsed["percent"].as_f64().expect("a number");
            assert!((0.0..=1.0).contains(&percent), "out of range: {percent}");
        }
    }

    #[test]
    fn tiny_advances_are_not_reported() {
        let mut reporter = ProgressReporter::new();
        let mut out = Vec::new();

        reporter.report(&mut out, 0.10, "encoding");
        assert!(
            !reporter.should_report(0.1001, "encoding"),
            "too small a step"
        );
        assert!(reporter.should_report(0.11, "encoding"), "a visible step");
        // A changed message always gets through, however small the step.
        assert!(reporter.should_report(0.1001, "muxing"));
    }

    #[test]
    fn the_first_report_always_goes_out() {
        let reporter = ProgressReporter::new();
        assert!(reporter.should_report(0.0, "starting"));
    }
}
