// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The shells a local tab can open by id, besides the platform default.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use tauminus_domain::LocalShell;
use tauminus_pty::{KnownShell, ShellListing};
use tauri::State;

use crate::ipc::{CommandError, ErrorCode, LocalShellView, LocalShellsView};

/// Finds the shells: [`tauminus_pty::known_shells`] outside tests.
type Lister = Arc<dyn Fn() -> ShellListing + Send + Sync>;

/// The shells a local tab can open by id, as last listed. The webview names
/// a shell by id only: what runs comes from this list, never from the
/// webview.
#[derive(Clone)]
pub(crate) struct ShellCatalog {
    list: Lister,
    /// The shells as last listed; `None` until the first listing.
    listed: Arc<Mutex<Option<Vec<KnownShell>>>>,
    /// Held while listing, so one listing runs at a time.
    listing: Arc<Mutex<()>>,
}

impl Default for ShellCatalog {
    fn default() -> Self {
        Self::new(Arc::new(tauminus_pty::known_shells))
    }
}

impl fmt::Debug for ShellCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellCatalog").finish_non_exhaustive()
    }
}

impl ShellCatalog {
    fn new(list: Lister) -> Self {
        Self {
            list,
            listed: Arc::default(),
            listing: Arc::default(),
        }
    }

    /// Lists the shells again, and says whether the list is whole. When WSL
    /// does not answer in time its distributions are slow, not gone: the
    /// ones listed before stay. Blocks; see [`tauminus_pty::known_shells`].
    pub(crate) fn refresh(&self) -> (Vec<KnownShell>, bool) {
        let _one = self.listing.lock().unwrap_or_else(PoisonError::into_inner);
        let ShellListing {
            mut shells,
            wsl_answered,
        } = (self.list)();
        // A name that reorders the text around it could pass for another.
        shells.retain(|known| !known.name.chars().any(crate::hosts::is_bidi_control));
        let mut listed = self.listed.lock().unwrap_or_else(PoisonError::into_inner);
        if !wsl_answered {
            let before = listed
                .iter()
                .flatten()
                .filter(|known| known.id.starts_with("wsl:"));
            shells.extend(before.cloned());
        }
        *listed = Some(shells.clone());
        (shells, wsl_answered)
    }

    /// The shell listed under `id`. A miss lists once more: the shell may
    /// be new, or WSL may have been slow when it was last listed.
    pub(crate) fn find(&self, id: &str) -> Option<LocalShell> {
        let pick = |shells: &[KnownShell]| {
            shells
                .iter()
                .find(|known| known.id == id)
                .map(|known| known.shell.clone())
        };
        let listed = self
            .listed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        listed
            .as_deref()
            .and_then(pick)
            .or_else(|| pick(&self.refresh().0))
    }
}

/// The shells this computer has besides the default, found again on each
/// call so one installed since shows up. Empty off Windows.
#[tauri::command]
#[specta::specta]
pub(crate) async fn local_shells(
    catalog: State<'_, ShellCatalog>,
) -> Result<LocalShellsView, CommandError> {
    let catalog = catalog.inner().clone();
    let (shells, complete) = tauri::async_runtime::spawn_blocking(move || catalog.refresh())
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(LocalShellsView {
        shells: shells
            .into_iter()
            .map(|known| LocalShellView {
                id: known.id,
                name: known.name,
            })
            .collect(),
        complete,
    })
}

#[cfg(test)]
mod tests;
