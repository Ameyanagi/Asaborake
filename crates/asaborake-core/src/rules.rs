//! Choosing how to treat a recording, from what is known about it.
//!
//! Per-channel settings cover the case where a whole channel wants treating
//! differently. Rules cover the rest: a drama in high definition deserves a
//! better profile than a five-minute news bulletin, and a programme somebody
//! is waiting for deserves to jump the queue.
//!
//! This is Amatsukaze's auto-select rule engine, reduced to the parts that can
//! be decided from what a recording actually tells us. Matching on ARIB genre
//! is missing because `asaborake-ts` does not parse genre yet; everything else
//! it matches on is here.
//!
//! First match wins, in file order, so the file reads as a list of exceptions
//! with the general case last — which is how somebody writing one thinks.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Error;

/// What is known about a recording when the rules are consulted.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    /// Channel id, as `EPGStation` sends it.
    pub channel_id: Option<String>,
    /// Programme title.
    pub title: Option<String>,
    /// Source path.
    pub path: Option<String>,
    /// Coded picture height, once the source has been probed.
    pub height: Option<u32>,
}

/// One rule: what to match, and what to do about it.
///
/// Every condition left unset matches anything, so a rule with no conditions
/// is the catch-all — useful as the last entry, and a mistake anywhere else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// What this rule is for, so a file of them can be read.
    #[serde(default)]
    pub name: Option<String>,

    /// Match only this channel.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// Match when the title contains this, ignoring case.
    #[serde(default)]
    pub title_contains: Option<String>,
    /// Match when the source path contains this, ignoring case.
    #[serde(default)]
    pub path_contains: Option<String>,
    /// Match only pictures at least this tall.
    #[serde(default)]
    pub min_height: Option<u32>,

    /// Encoding profile to use.
    #[serde(default)]
    pub profile: Option<String>,
    /// Queue priority; higher runs first.
    #[serde(default)]
    pub priority: Option<i64>,
    /// Whether to look for commercials.
    #[serde(default)]
    pub detect_commercials: Option<bool>,
}

impl Rule {
    /// Whether this rule applies to `candidate`.
    #[must_use]
    pub fn matches(&self, candidate: &Candidate) -> bool {
        // Comparison is case-insensitive because a title is written by a
        // broadcaster and a rule by a person, and neither is thinking about
        // the other's capitalisation.
        let contains = |haystack: Option<&String>, needle: &str| {
            haystack.is_some_and(|value| value.to_lowercase().contains(&needle.to_lowercase()))
        };

        if let Some(channel) = &self.channel_id
            && candidate.channel_id.as_deref() != Some(channel.as_str())
        {
            return false;
        }
        if let Some(needle) = &self.title_contains
            && !contains(candidate.title.as_ref(), needle)
        {
            return false;
        }
        if let Some(needle) = &self.path_contains
            && !contains(candidate.path.as_ref(), needle)
        {
            return false;
        }
        if let Some(minimum) = self.min_height
            && candidate.height.is_none_or(|height| height < minimum)
        {
            return false;
        }
        true
    }
}

/// The rules, in the order they are tried.
#[derive(Debug, Clone)]
pub struct RuleSet {
    path: PathBuf,
}

impl RuleSet {
    /// Open the rules at `path`, which need not exist.
    #[must_use]
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Every rule, in file order.
    ///
    /// A file that cannot be read or parsed yields no rules and a warning: no
    /// rules is what the engine did before this existed, and a typo in a
    /// hand-edited file should not stop the queue.
    #[must_use]
    pub fn all(&self) -> Vec<Rule> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        match serde_json::from_str(&text) {
            Ok(rules) => rules,
            Err(error) => {
                tracing::warn!(%error, path = %self.path.display(), "cannot read the rules");
                Vec::new()
            }
        }
    }

    /// The first rule that applies, if any.
    #[must_use]
    pub fn first_match(&self, candidate: &Candidate) -> Option<Rule> {
        self.all().into_iter().find(|rule| rule.matches(candidate))
    }

    /// Replace the whole list.
    ///
    /// All of them at once because their *order* is part of their meaning, and
    /// editing one in isolation cannot express a change of order.
    ///
    /// # Errors
    /// Returns [`Error::Io`] if the file cannot be written, or
    /// [`Error::SidecarEncode`] if the rules cannot be serialised.
    pub fn replace(&self, rules: &[Rule]) -> Result<(), Error> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_string_pretty(rules).map_err(Error::SidecarEncode)?;
        std::fs::write(&self.path, json).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drama() -> Candidate {
        Candidate {
            channel_id: Some("1040".to_owned()),
            title: Some("最終回スペシャル ドラマ".to_owned()),
            path: Some("/recordings/drama-ep12.ts".to_owned()),
            height: Some(1080),
        }
    }

    #[test]
    fn a_rule_with_no_conditions_matches_anything() {
        // Which is what makes it useful as the last entry, and a mistake
        // anywhere else.
        assert!(Rule::default().matches(&drama()));
        assert!(Rule::default().matches(&Candidate::default()));
    }

    #[test]
    fn every_condition_has_to_hold() {
        let rule = Rule {
            channel_id: Some("1040".to_owned()),
            title_contains: Some("ドラマ".to_owned()),
            min_height: Some(720),
            ..Rule::default()
        };
        assert!(rule.matches(&drama()));

        // Any one of them failing is enough.
        let wrong_channel = Candidate {
            channel_id: Some("1024".to_owned()),
            ..drama()
        };
        assert!(!rule.matches(&wrong_channel));

        let standard_definition = Candidate {
            height: Some(480),
            ..drama()
        };
        assert!(!rule.matches(&standard_definition));
    }

    #[test]
    fn matching_ignores_capitalisation() {
        // A title is written by a broadcaster and a rule by a person, and
        // neither is thinking about the other's capitalisation.
        let rule = Rule {
            title_contains: Some("news".to_owned()),
            ..Rule::default()
        };
        let candidate = Candidate {
            title: Some("Evening NEWS at Ten".to_owned()),
            ..Candidate::default()
        };
        assert!(rule.matches(&candidate));
    }

    #[test]
    fn a_condition_about_something_unknown_does_not_match() {
        // Better to fall through to a more general rule than to guess.
        let rule = Rule {
            min_height: Some(720),
            ..Rule::default()
        };
        assert!(!rule.matches(&Candidate::default()));

        let rule = Rule {
            title_contains: Some("anything".to_owned()),
            ..Rule::default()
        };
        assert!(!rule.matches(&Candidate::default()));
    }

    #[test]
    fn the_first_matching_rule_is_the_one_that_applies() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rules = RuleSet::open(dir.path().join("rules.json"));
        rules
            .replace(&[
                Rule {
                    name: Some("HD drama".to_owned()),
                    title_contains: Some("ドラマ".to_owned()),
                    min_height: Some(720),
                    profile: Some("nvenc-hevc".to_owned()),
                    priority: Some(10),
                    ..Rule::default()
                },
                Rule {
                    name: Some("everything else".to_owned()),
                    profile: Some("nvenc-h264".to_owned()),
                    ..Rule::default()
                },
            ])
            .expect("writes");

        let hit = rules.first_match(&drama()).expect("a rule");
        assert_eq!(hit.profile.as_deref(), Some("nvenc-hevc"));
        assert_eq!(hit.priority, Some(10));

        // Anything the first rule does not claim falls to the catch-all.
        let news = Candidate {
            title: Some("ニュース".to_owned()),
            height: Some(1080),
            ..Candidate::default()
        };
        let hit = rules.first_match(&news).expect("a rule");
        assert_eq!(hit.profile.as_deref(), Some("nvenc-h264"));
    }

    #[test]
    fn no_rules_at_all_is_a_valid_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rules = RuleSet::open(dir.path().join("missing.json"));
        assert!(rules.all().is_empty());
        assert_eq!(rules.first_match(&drama()), None);
    }

    #[test]
    fn a_file_that_will_not_parse_does_not_stop_the_queue() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("rules.json");
        std::fs::write(&path, "[ not json").expect("writes");

        assert!(RuleSet::open(&path).all().is_empty());
    }
}
