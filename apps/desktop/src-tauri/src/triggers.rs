// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Highlight sets over IPC: the library, checking a pattern as it is
//! typed, and matching a pane's visible lines.
//!
//! The rules and the regex engine stay here; the view only sends text and
//! draws what comes back. A pane prepares once for its host and scans with
//! the handle it got, so a scan reads no host and compiles nothing.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::{TriggerBook, TriggerBookError};
use tauminus_domain::{
    Action, Colour, Highlight, HostId, Label, MAX_SCAN_BYTES, MAX_SCAN_LINES, RulePattern,
    TriggerError, TriggerLibrary, TriggerRule, TriggerSet, TriggerSetId,
};
use tauri::State;

use crate::hosts::Hosts;
use crate::ipc::{CommandError, ErrorCode};

/// The trigger library, or why it could not be opened. A damaged file is
/// kept as it is and reported; the rest of the app works without it.
pub(crate) enum Triggers {
    Ready(Arc<TriggerBook>),
    Unavailable(String),
}

impl Triggers {
    pub(crate) fn book(&self) -> Result<&TriggerBook, CommandError> {
        match self {
            Self::Ready(book) => Ok(book),
            Self::Unavailable(reason) => {
                Err(CommandError::new(ErrorCode::StoreFailed, reason.clone()))
            }
        }
    }
}

impl From<TriggerBookError> for CommandError {
    fn from(error: TriggerBookError) -> Self {
        let code = match &error {
            TriggerBookError::Trigger(TriggerError::NoSuch(_)) => ErrorCode::NotFound,
            TriggerBookError::Trigger(TriggerError::TooManySets | TriggerError::TooManyRules) => {
                ErrorCode::LimitReached
            }
            TriggerBookError::Trigger(_) | TriggerBookError::Id(_) => ErrorCode::InvalidArgument,
            TriggerBookError::Store(_) => ErrorCode::StoreFailed,
        };
        Self::new(code, error.to_string())
    }
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

/// How a rule colours. Colours are `"0"` to `"15"` for the theme's
/// palette, or `"#rrggbb"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StyleSpec {
    #[serde(default)]
    pub(crate) fg: Option<String>,
    #[serde(default)]
    pub(crate) bg: Option<String>,
    /// The whole line, not only the match.
    #[serde(default)]
    pub(crate) line: bool,
}

impl From<Highlight> for StyleSpec {
    fn from(style: Highlight) -> Self {
        Self {
            fg: style.fg().map(|c| c.to_string()),
            bg: style.bg().map(|c| c.to_string()),
            line: style.line(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuleSpec {
    pub(crate) name: String,
    pub(crate) pattern: String,
    pub(crate) enabled: bool,
    pub(crate) style: StyleSpec,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct TriggerSetView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) rules: Vec<RuleSpec>,
    /// Where it stands among the sets every session gets, below what hosts
    /// and groups name, 0 first; `null` when it is not one of them.
    pub(crate) rank: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TriggerSetInput {
    /// Absent for a new set.
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    pub(crate) rules: Vec<RuleSpec>,
}

/// By name. O(s log s + r).
pub(crate) fn view(library: &TriggerLibrary) -> Vec<TriggerSetView> {
    let mut all: Vec<TriggerSetView> = library
        .sets()
        .map(|set| TriggerSetView {
            id: set.id.to_string(),
            name: set.name.as_str().to_owned(),
            rules: set
                .rules()
                .iter()
                .map(|rule| {
                    let Action::Highlight(style) = rule.action;
                    RuleSpec {
                        name: rule.name.as_str().to_owned(),
                        pattern: rule.pattern.as_str().to_owned(),
                        enabled: rule.enabled,
                        style: style.into(),
                    }
                })
                .collect(),
            rank: library
                .defaults()
                .iter()
                .position(|d| d == &set.id)
                .and_then(|at| u32::try_from(at).ok()),
        })
        .collect();
    all.sort_by_cached_key(|s| (s.name.to_lowercase(), s.id.clone()));
    all
}

fn colour(text: Option<String>) -> Result<Option<Colour>, CommandError> {
    text.filter(|t| !t.is_empty())
        .map(|t| Colour::parse(&t))
        .transpose()
        .map_err(invalid)
}

/// The id a saved set keeps, or a new one.
fn id_for(book: &TriggerBook, input: &TriggerSetInput) -> Result<TriggerSetId, CommandError> {
    match &input.id {
        Some(id) => TriggerSetId::parse(id),
        None => book.new_id(),
    }
    .map_err(invalid)
}

/// Checks and compiles every rule; an error names the rule. Compiling takes
/// a moment: run it off the async threads.
fn parse(id: TriggerSetId, input: TriggerSetInput) -> Result<TriggerSet, CommandError> {
    let rules = input
        .rules
        .into_iter()
        .enumerate()
        .map(|(at, rule)| {
            let named = |e: &dyn std::fmt::Display| invalid(format!("rule {}: {e}", at + 1));
            let style = Highlight::new(
                colour(rule.style.fg)?,
                colour(rule.style.bg)?,
                rule.style.line,
            )
            .map_err(|e| named(&e))?;
            Ok(TriggerRule {
                name: Label::parse(&rule.name).map_err(|e| named(&e))?,
                pattern: RulePattern::checked(&rule.pattern).map_err(|e| named(&e))?,
                action: Action::Highlight(style),
                enabled: rule.enabled,
            })
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    TriggerSet::new(id, Label::parse(&input.name).map_err(invalid)?, rules)
        .map_err(|e| TriggerBookError::from(e).into())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_sets_list(
    triggers: State<'_, Triggers>,
) -> Result<Vec<TriggerSetView>, CommandError> {
    Ok(view(&triggers.book()?.snapshot().await))
}

/// Adds a set with a new id, or replaces the saved one `id` names. An id
/// the caller names must be saved already: a deleted set that hosts still
/// list does not come back under its old id.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_set_save(
    triggers: State<'_, Triggers>,
    set: TriggerSetInput,
) -> Result<Vec<TriggerSetView>, CommandError> {
    let book = triggers.book()?;
    let replaces = set.id.is_some();
    let id = id_for(book, &set)?;
    let set = tauri::async_runtime::spawn_blocking(move || parse(id, set))
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))??;
    Ok(view(
        &book
            .edit(|library| {
                if replaces && library.get(&set.id).is_none() {
                    return Err(TriggerError::NoSuch(set.id.clone()));
                }
                library.put(set)
            })
            .await?,
    ))
}

/// Removes a set. Hosts and groups that name it skip it from then on.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_set_delete(
    triggers: State<'_, Triggers>,
    id: String,
) -> Result<Vec<TriggerSetView>, CommandError> {
    let id = TriggerSetId::parse(&id).map_err(invalid)?;
    Ok(view(
        &triggers
            .book()?
            .edit(|library| library.remove(&id).map(drop))
            .await?,
    ))
}

/// The sets every session gets, in precedence order.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_defaults_save(
    triggers: State<'_, Triggers>,
    ids: Vec<String>,
) -> Result<Vec<TriggerSetView>, CommandError> {
    let ids = ids
        .iter()
        .map(|id| TriggerSetId::parse(id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    Ok(view(
        &triggers
            .book()?
            .edit(|library| library.set_defaults(ids))
            .await?,
    ))
}

/// Why `pattern` cannot be a rule's, or `null` when it can; for the editor
/// to say as it is typed.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_pattern_check(pattern: String) -> Option<String> {
    // Compiling takes a moment: off the async threads.
    tauri::async_runtime::spawn_blocking(move || {
        RulePattern::checked(&pattern).err().map(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Some(e.to_string()))
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct PreparedView {
    pub(crate) handle: u32,
    /// By the index hits carry; empty when no rule applies, and the pane
    /// need not scan.
    pub(crate) styles: Vec<StyleSpec>,
}

/// Compiles the rules for a pane: those of saved host `host` and its
/// groups, then the defaults; only the defaults for a pane with no saved
/// host.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_prepare(
    hosts: State<'_, Hosts>,
    triggers: State<'_, Triggers>,
    host: Option<String>,
) -> Result<PreparedView, CommandError> {
    let named = match host {
        Some(host) => {
            let host = HostId::parse(&host).map_err(invalid)?;
            hosts
                .book()?
                .triggers(&host)
                .await
                .map_err(|e| CommandError::new(ErrorCode::NotFound, e.to_string()))?
        }
        None => Vec::new(),
    };
    let prepared = triggers.book()?.prepare(named).await?;
    Ok(PreparedView {
        handle: prepared.handle,
        styles: prepared.styles.into_iter().map(StyleSpec::from).collect(),
    })
}

/// Where a rule matched: line `line` of the scan, code points `start` to
/// `end`, coloured as style `style`.
#[derive(Debug, Clone, Copy, Serialize, Type)]
pub(crate) struct HitView {
    pub(crate) line: u32,
    pub(crate) style: u16,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

/// Matches `lines` with the rules prepared as `handle`; `null` when the
/// rules changed since, and the pane should prepare again. At most
/// [`MAX_SCAN_LINES`] lines; the matcher reads [`MAX_SCAN_BYTES`] of them.
#[tauri::command]
#[specta::specta]
pub(crate) async fn trigger_scan(
    triggers: State<'_, Triggers>,
    handle: u32,
    lines: Vec<String>,
) -> Result<Option<Vec<HitView>>, CommandError> {
    // The view sends at most this; the matcher reads less, cutting lines
    // and stopping at its budget.
    let bytes: usize = lines.iter().map(String::len).sum();
    if lines.len() > MAX_SCAN_LINES || bytes > 4 * MAX_SCAN_BYTES {
        return Err(invalid("too much text to scan at once"));
    }
    let book = Arc::clone(match &*triggers {
        Triggers::Ready(book) => book,
        Triggers::Unavailable(_) => return Ok(Some(Vec::new())),
    });
    // Matching is linear in the text, but it is CPU work: off the async
    // threads.
    let hits = tauri::async_runtime::spawn_blocking(move || {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        book.scan(handle, &refs)
    })
    .await
    .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(hits.map(|hits| {
        hits.into_iter()
            .map(|h| HitView {
                line: h.line,
                style: h.style,
                start: h.start,
                end: h.end,
            })
            .collect()
    }))
}

#[cfg(test)]
mod tests;
