// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The command history over IPC. The view sends each command it saw run;
//! the backend decides whether the host records history and where the
//! entry goes. Searching needs the vault open.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::history::{History, HistoryError, Recorded};
use tauminus_domain::{ExitFilter, HistoryEntry, HistoryHit, HistoryQuery};
use tauri::State;

use crate::ipc::{CommandError, ErrorCode};

impl From<HistoryError> for CommandError {
    fn from(error: HistoryError) -> Self {
        let code = match &error {
            HistoryError::Locked => ErrorCode::VaultLocked,
            HistoryError::NoVault => ErrorCode::NotFound,
            HistoryError::Invalid(_) => ErrorCode::InvalidArgument,
            HistoryError::Store(_) | HistoryError::Stale => ErrorCode::StoreFailed,
            HistoryError::Busy(_) => ErrorCode::Unavailable,
        };
        Self::new(code, error.to_string())
    }
}

/// A command the view saw run.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryRecordSpec {
    /// The saved host, or `null` for a local shell or an ad hoc connect.
    #[serde(default)]
    pub(crate) host: Option<String>,
    pub(crate) label: String,
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) output: String,
    #[serde(default)]
    pub(crate) exit: Option<i32>,
    /// When the command started, in seconds since the Unix epoch; the
    /// entry is recorded later, when its output ends. Within the last day,
    /// or now.
    #[serde(default)]
    pub(crate) started: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RecordedSpec {
    Stored,
    Sealed,
    Dropped,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ExitFilterSpec {
    #[default]
    Any,
    Success,
    Failure,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryQuerySpec {
    #[serde(default)]
    pub(crate) text: String,
    /// A saved host's id; `""` for local and ad hoc sessions; `null` for all.
    #[serde(default)]
    pub(crate) host: Option<String>,
    #[serde(default)]
    pub(crate) from_ms: Option<f64>,
    #[serde(default)]
    pub(crate) to_ms: Option<f64>,
    #[serde(default)]
    pub(crate) exit: ExitFilterSpec,
    pub(crate) limit: u32,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryHitSpec {
    /// Pass to `history_output` for the full output.
    pub(crate) id: u32,
    pub(crate) host: Option<String>,
    pub(crate) label: String,
    /// Seconds since the Unix epoch.
    pub(crate) at: u32,
    pub(crate) exit: Option<i32>,
    pub(crate) command: String,
    /// Matched output, with matches between U+0001 and U+0002.
    pub(crate) snippet: String,
}

impl From<HistoryHit> for HistoryHitSpec {
    fn from(hit: HistoryHit) -> Self {
        Self {
            // Rows past 4 billion would need a new id type; that is decades
            // of commands at any typing speed.
            id: u32::try_from(hit.id).unwrap_or(u32::MAX),
            host: hit.host,
            label: hit.label,
            at: u32::try_from(hit.at_ms / 1000).unwrap_or(u32::MAX),
            exit: hit.exit,
            command: hit.command,
            snippet: hit.snippet,
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a time from the view is whole milliseconds well inside i64"
)]
fn millis(value: Option<f64>) -> Option<i64> {
    value.filter(|v| v.is_finite()).map(|v| v as i64)
}

/// The view's start time, kept within the last day so a wrong clock in the
/// view cannot date an entry into the future or the distant past.
fn started_at(started: Option<u32>, now_ms: i64) -> i64 {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    started.map_or(now_ms, |s| {
        (i64::from(s) * 1000).clamp(now_ms - DAY_MS, now_ms)
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

type HistoryState<'a> = State<'a, Arc<History>>;

/// Keeps a command, unless its host has history turned off.
#[tauri::command]
#[specta::specta]
pub(crate) async fn history_record(
    history: HistoryState<'_>,
    hosts: State<'_, crate::hosts::Hosts>,
    gate: State<'_, crate::sync::Gate>,
    entry: HistoryRecordSpec,
) -> Result<RecordedSpec, CommandError> {
    if let Some(host) = &entry.host {
        let id = tauminus_domain::HostId::parse(host)
            .map_err(|e| CommandError::new(ErrorCode::InvalidArgument, e.to_string()))?;
        // A host deleted since the session opened records nothing.
        let records = hosts.book()?.records_history(&id).await.unwrap_or(false)
            // A host another device changed, unseen, records nothing.
            && gate.passes(&hosts, &id).await;
        if !records {
            return Ok(RecordedSpec::Dropped);
        }
    }
    let now = now_ms();
    let at_ms = started_at(entry.started, now);
    let recorded = history
        .record(HistoryEntry {
            host: entry.host,
            label: entry.label,
            at_ms,
            command: entry.command,
            output: entry.output,
            exit: entry.exit,
        })
        .await?;
    Ok(match recorded {
        Recorded::Stored => RecordedSpec::Stored,
        Recorded::Sealed => RecordedSpec::Sealed,
        Recorded::Dropped => RecordedSpec::Dropped,
    })
}

/// Newest first. Needs the vault open.
#[tauri::command]
#[specta::specta]
pub(crate) async fn history_search(
    history: HistoryState<'_>,
    query: HistoryQuerySpec,
) -> Result<Vec<HistoryHitSpec>, CommandError> {
    let query = HistoryQuery {
        text: query.text,
        host: query.host,
        from_ms: millis(query.from_ms),
        to_ms: millis(query.to_ms),
        exit: match query.exit {
            ExitFilterSpec::Any => ExitFilter::Any,
            ExitFilterSpec::Success => ExitFilter::Success,
            ExitFilterSpec::Failure => ExitFilter::Failure,
        },
        limit: query.limit,
    };
    Ok(history
        .search(query)
        .await?
        .into_iter()
        .map(HistoryHitSpec::from)
        .collect())
}

/// One command's full output.
#[tauri::command]
#[specta::specta]
pub(crate) async fn history_output(
    history: HistoryState<'_>,
    id: u32,
) -> Result<Option<String>, CommandError> {
    Ok(history.output(i64::from(id)).await?)
}

/// Forgets one saved host's commands (`""` for local sessions), or all.
/// Returns how many went.
#[tauri::command]
#[specta::specta]
pub(crate) async fn history_clear(
    history: HistoryState<'_>,
    host: Option<String>,
) -> Result<u32, CommandError> {
    let removed = history.clear(host).await?;
    Ok(u32::try_from(removed).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests;
