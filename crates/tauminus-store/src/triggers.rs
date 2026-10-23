// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The highlight and trigger file format, version 1. As with hosts, an
//! older build refuses a newer file instead of dropping what it does not
//! know.

use serde::{Deserialize, Serialize};
use tauminus_domain::{
    Action, Colour, Highlight, Label, MAX_SET_RULES, MAX_SETS, RulePattern, SendText,
    TriggerLibrary, TriggerRule, TriggerSet, TriggerSetId,
};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    sets: Vec<SetRecord>,
    /// Set ids applied to every session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    defaults: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetRecord {
    id: String,
    name: String,
    #[serde(default)]
    rules: Vec<RuleRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleRecord {
    name: String,
    pattern: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    enabled: bool,
    action: ActionRecord,
}

fn yes() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum ActionRecord {
    Highlight {
        /// `"0"` to `"15"` for a palette colour, or `"#rrggbb"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fg: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bg: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        line: bool,
    },
    Notify,
    Capture,
    Send {
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter: bool,
    },
}

impl From<&Action> for ActionRecord {
    fn from(action: &Action) -> Self {
        match action {
            Action::Highlight(style) => Self::Highlight {
                fg: style.fg().map(|c| c.to_string()),
                bg: style.bg().map(|c| c.to_string()),
                line: style.line(),
            },
            Action::Notify => Self::Notify,
            Action::Capture => Self::Capture,
            Action::Send(send) => Self::Send {
                text: send.text().to_owned(),
                enter: send.enter(),
            },
        }
    }
}

/// O(total pattern size).
pub(crate) fn to_json(library: &TriggerLibrary) -> Result<Vec<u8>, serde_json::Error> {
    let file = File {
        version: VERSION,
        sets: library
            .sets()
            .map(|set| SetRecord {
                id: set.id.to_string(),
                name: set.name.as_str().to_owned(),
                rules: set
                    .rules()
                    .iter()
                    .map(|rule| RuleRecord {
                        name: rule.name.as_str().to_owned(),
                        pattern: rule.pattern.as_str().to_owned(),
                        enabled: rule.enabled,
                        action: ActionRecord::from(&rule.action),
                    })
                    .collect(),
            })
            .collect(),
        defaults: library.defaults().iter().map(ToString::to_string).collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&file)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Checks every set and rule as the domain types do. Patterns are checked
/// for syntax, not compiled: reading costs the file's length. O(bytes).
pub(crate) fn from_json(bytes: &[u8]) -> Result<TriggerLibrary, String> {
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "it is version {}, and this build reads version {VERSION}; update Tauminus",
            file.version
        ));
    }
    // Counted before anything else is read: the file may come from
    // elsewhere, and its size alone must not decide the work.
    if file.sets.len() > MAX_SETS || file.sets.iter().any(|s| s.rules.len() > MAX_SET_RULES) {
        return Err(format!(
            "at most {MAX_SETS} sets of {MAX_SET_RULES} rules each"
        ));
    }
    let err = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    let mut seen = std::collections::BTreeSet::new();
    let sets = file
        .sets
        .into_iter()
        .map(|s| {
            let id = TriggerSetId::parse(&s.id).map_err(|e| err("set id", &e))?;
            if !seen.insert(id.clone()) {
                return Err(format!("set {id} is listed twice"));
            }
            let name = Label::parse(&s.name).map_err(|e| err("set name", &e))?;
            let rules = s
                .rules
                .into_iter()
                .map(|r| rule(r).map_err(|e| format!("set {id}: {e}")))
                .collect::<Result<Vec<_>, String>>()?;
            TriggerSet::read(id, name, rules).map_err(|e| err("set", &e))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let defaults = file
        .defaults
        .iter()
        .map(|id| TriggerSetId::parse(id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| err("default set", &e))?;
    TriggerLibrary::new(sets, defaults).map_err(|e| e.to_string())
}

fn rule(record: RuleRecord) -> Result<TriggerRule, String> {
    let name = Label::parse(&record.name).map_err(|e| format!("rule name: {e}"))?;
    let pattern =
        RulePattern::parse(&record.pattern).map_err(|e| format!("rule {}: {e}", name.as_str()))?;
    let colour = |text: Option<String>| {
        text.map(|t| Colour::parse(&t).map_err(|e| format!("colour {t:?}: {e}")))
            .transpose()
    };
    let action = match record.action {
        ActionRecord::Highlight { fg, bg, line } => Action::Highlight(
            Highlight::new(colour(fg)?, colour(bg)?, line).map_err(|e| e.to_string())?,
        ),
        ActionRecord::Notify => Action::Notify,
        ActionRecord::Capture => Action::Capture,
        ActionRecord::Send { text, enter } => {
            Action::Send(SendText::new(text, enter).map_err(|e| e.to_string())?)
        }
    };
    Ok(TriggerRule {
        name,
        pattern,
        action,
        enabled: record.enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_reads_back_as_written() {
        let rule = |pattern: &str, enabled: bool, line: bool| TriggerRule {
            name: Label::parse(pattern).unwrap(),
            pattern: RulePattern::parse(pattern).unwrap(),
            action: Action::Highlight(
                Highlight::new(
                    Some(Colour::Palette(9)),
                    Some(Colour::Rgb(0, 0, 0x40)),
                    line,
                )
                .unwrap(),
            ),
            enabled,
        };
        let id = TriggerSetId::parse("errors").unwrap();
        let library = TriggerLibrary::new(
            vec![
                TriggerSet::new(
                    id.clone(),
                    Label::parse("Errors").unwrap(),
                    vec![
                        rule("ERROR", true, false),
                        rule(r"(?i)fail\w*", false, true),
                        TriggerRule {
                            action: Action::Notify,
                            ..rule("done", true, false)
                        },
                        TriggerRule {
                            action: Action::Capture,
                            ..rule("token", true, false)
                        },
                        TriggerRule {
                            action: Action::Send(SendText::new("yes".into(), true).unwrap()),
                            ..rule(r"\[y/N\]", true, false)
                        },
                    ],
                )
                .unwrap(),
            ],
            vec![id],
        )
        .unwrap();
        let bytes = to_json(&library).unwrap();
        assert_eq!(from_json(&bytes).unwrap(), library);
    }

    #[test]
    fn a_damaged_or_newer_file_says_what_is_wrong() {
        for (label, contents, says) in [
            ("newer", r#"{"version": 2}"#, "version 2"),
            (
                "bad pattern",
                r#"{"version": 1, "sets": [{"id": "a", "name": "a", "rules": [{"name": "r", "pattern": "(", "action": {"kind": "highlight", "fg": "1"}}]}]}"#,
                "rule r",
            ),
            (
                "no colour",
                r#"{"version": 1, "sets": [{"id": "a", "name": "a", "rules": [{"name": "r", "pattern": "x", "action": {"kind": "highlight"}}]}]}"#,
                "colour",
            ),
            (
                "twice",
                r#"{"version": 1, "sets": [{"id": "a", "name": "a"}, {"id": "a", "name": "b"}]}"#,
                "twice",
            ),
            (
                "control in sent text",
                r#"{"version": 1, "sets": [{"id": "a", "name": "a", "rules": [{"name": "r", "pattern": "x", "action": {"kind": "send", "text": "rm -rf /\u001b[201~"}}]}]}"#,
                "control",
            ),
            (
                "unknown action",
                r#"{"version": 1, "sets": [{"id": "a", "name": "a", "rules": [{"name": "r", "pattern": "x", "action": {"kind": "explode"}}]}]}"#,
                "explode",
            ),
        ] {
            let error = from_json(contents.as_bytes()).unwrap_err();
            assert!(error.contains(says), "{label}: {error}");
        }
    }
}

#[cfg(test)]
mod reading {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_file_of_patterns_slow_to_compile_reads_in_time_linear_in_its_length() {
        // `\w{20}` is 6 bytes and compiles to about 1.2 MiB: compiled on
        // load, 200 such sets took minutes and gigabytes.
        let rules: Vec<String> = (0..MAX_SET_RULES)
            .map(|i| {
                format!(
                    r#"{{"name": "r{i}", "pattern": "\\w{{20}}{i}", "action": {{"kind": "highlight", "fg": "1"}}}}"#
                )
            })
            .collect();
        let sets: Vec<String> = (0..MAX_SETS)
            .map(|i| {
                format!(
                    r#"{{"id": "s{i}", "name": "s{i}", "rules": [{}]}}"#,
                    rules.join(",")
                )
            })
            .collect();
        let file = format!(r#"{{"version": 1, "sets": [{}]}}"#, sets.join(","));
        let began = Instant::now();
        let library = from_json(file.as_bytes()).unwrap();
        assert_eq!(library.sets().count(), MAX_SETS);
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn more_sets_than_allowed_are_refused_before_any_is_read() {
        let sets: Vec<String> = (0..=MAX_SETS)
            .map(|i| format!(r#"{{"id": "s{i}", "name": "s{i}"}}"#))
            .collect();
        let file = format!(r#"{{"version": 1, "sets": [{}]}}"#, sets.join(","));
        assert!(from_json(file.as_bytes()).unwrap_err().contains("at most"));
    }
}
