// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saved snippets over IPC: the library, and the text to type to run one.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::{SnippetBook, SnippetBookError};
use tauminus_domain::{HostId, Label, Library, Snippet, SnippetError, SnippetId};
use tauri::State;

use crate::hosts::Hosts;
use crate::ipc::{CommandError, ErrorCode};

/// The snippet library, or why it could not be opened. A damaged file is
/// kept as it is and reported; the rest of the app works without it.
pub(crate) enum Snippets {
    Ready(Arc<SnippetBook>),
    Unavailable(String),
}

impl Snippets {
    pub(crate) fn book(&self) -> Result<&SnippetBook, CommandError> {
        match self {
            Self::Ready(book) => Ok(book),
            Self::Unavailable(reason) => {
                Err(CommandError::new(ErrorCode::StoreFailed, reason.clone()))
            }
        }
    }
}

impl From<SnippetBookError> for CommandError {
    fn from(error: SnippetBookError) -> Self {
        let code = match &error {
            SnippetBookError::Snippet(SnippetError::NoSuch(_)) => ErrorCode::NotFound,
            SnippetBookError::Snippet(SnippetError::TooMany) => ErrorCode::LimitReached,
            SnippetBookError::Snippet(_) | SnippetBookError::Id(_) => ErrorCode::InvalidArgument,
            SnippetBookError::Store(_) => ErrorCode::StoreFailed,
        };
        Self::new(code, error.to_string())
    }
}

fn invalid(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

fn snippet_error(error: SnippetError) -> CommandError {
    SnippetBookError::from(error).into()
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct VariableView {
    pub(crate) name: String,
    pub(crate) default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct SnippetView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) folder: Option<String>,
    pub(crate) body: String,
    /// Its placeholders in first-use order, for the form that fills them.
    pub(crate) variables: Vec<VariableView>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnippetInput {
    /// Absent for a new snippet.
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) folder: Option<String>,
    pub(crate) body: String,
}

/// What a host's startup snippets type, and those skipped with why.
#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct StartupView {
    pub(crate) text: String,
    pub(crate) skipped: Vec<SkippedView>,
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct SkippedView {
    pub(crate) id: String,
    pub(crate) reason: String,
}

/// By folder, then name. O(s log s).
pub(crate) fn view(library: &Library) -> Vec<SnippetView> {
    let mut all: Vec<SnippetView> = library
        .snippets()
        .map(|s| SnippetView {
            id: s.id.to_string(),
            name: s.name.as_str().to_owned(),
            folder: s.folder.as_ref().map(|f| f.as_str().to_owned()),
            body: s.body().to_owned(),
            variables: s
                .variables()
                .into_iter()
                .map(|v| VariableView {
                    name: v.name,
                    default: v.default,
                })
                .collect(),
        })
        .collect();
    all.sort_by_cached_key(|s| (s.folder.clone(), s.name.to_lowercase(), s.id.clone()));
    all
}

pub(crate) fn parse(book: &SnippetBook, input: SnippetInput) -> Result<Snippet, CommandError> {
    let id = match input.id {
        Some(id) => SnippetId::parse(&id),
        None => book.new_id(),
    }
    .map_err(invalid)?;
    let folder = input
        .folder
        .filter(|f| !f.trim().is_empty())
        .map(|f| Label::parse(&f))
        .transpose()
        .map_err(invalid)?;
    Snippet::new(
        id,
        Label::parse(&input.name).map_err(invalid)?,
        folder,
        input.body,
    )
    .map_err(snippet_error)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn snippets_list(
    snippets: State<'_, Snippets>,
) -> Result<Vec<SnippetView>, CommandError> {
    Ok(view(&snippets.book()?.snapshot().await))
}

/// Adds a snippet with a new id, or replaces the saved one `id` names.
#[tauri::command]
#[specta::specta]
pub(crate) async fn snippet_save(
    snippets: State<'_, Snippets>,
    snippet: SnippetInput,
) -> Result<Vec<SnippetView>, CommandError> {
    let book = snippets.book()?;
    Ok(view(&save(book, snippet).await?))
}

/// An id the caller names must be saved already: a deleted snippet that
/// hosts still list does not come back under its old id.
pub(crate) async fn save(book: &SnippetBook, input: SnippetInput) -> Result<Library, CommandError> {
    let replaces = input.id.is_some();
    let snippet = parse(book, input)?;
    Ok(book
        .edit(|library| {
            if replaces && library.get(&snippet.id).is_none() {
                return Err(SnippetError::NoSuch(snippet.id.clone()));
            }
            library.put(snippet)
        })
        .await?)
}

/// Removes a snippet. Hosts that start with it skip it from then on.
#[tauri::command]
#[specta::specta]
pub(crate) async fn snippet_delete(
    snippets: State<'_, Snippets>,
    id: String,
) -> Result<Vec<SnippetView>, CommandError> {
    let book = snippets.book()?;
    let id = SnippetId::parse(&id).map_err(invalid)?;
    Ok(view(
        &book.edit(|library| library.remove(&id).map(drop)).await?,
    ))
}

/// The text to type to run a snippet: placeholders filled from `values`
/// or their defaults, each line ended by Enter.
#[tauri::command]
#[specta::specta]
pub(crate) async fn snippet_command(
    snippets: State<'_, Snippets>,
    id: String,
    values: BTreeMap<String, String>,
) -> Result<String, CommandError> {
    let id = SnippetId::parse(&id).map_err(invalid)?;
    snippets
        .book()?
        .command(&id, &values)
        .await
        .map_err(snippet_error)
}

/// What to type into a new shell on a saved host.
#[tauri::command]
#[specta::specta]
pub(crate) async fn host_startup(
    hosts: State<'_, Hosts>,
    snippets: State<'_, Snippets>,
    host: String,
) -> Result<StartupView, CommandError> {
    let host = HostId::parse(&host).map_err(invalid)?;
    let ids = hosts
        .book()?
        .startup(&host)
        .await
        .map_err(|e| CommandError::new(ErrorCode::NotFound, e.to_string()))?;
    if ids.is_empty() {
        return Ok(StartupView {
            text: String::new(),
            skipped: Vec::new(),
        });
    }
    let (text, skipped) = snippets.book()?.startup(&ids).await;
    Ok(StartupView {
        text,
        skipped: skipped
            .into_iter()
            .map(|s| SkippedView {
                id: s.id.to_string(),
                reason: s.reason,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests;
