// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The shells a local tab can open by id, besides the platform default.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// The shells as last listed, and whether WSL answered in that listing;
    /// `None` until the first listing.
    listed: Arc<Mutex<Option<ShellListing>>>,
    /// Held while listing, so one listing runs at a time.
    listing: Arc<Mutex<()>>,
    /// Listings finished so far, so a caller that waited for one can tell.
    generation: Arc<AtomicU64>,
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
            generation: Arc::default(),
        }
    }

    /// Lists the shells again, and says whether the list is whole. When WSL
    /// does not answer in time its distributions are slow, not gone: the
    /// ones listed before stay. Blocks; see [`tauminus_pty::known_shells`].
    pub(crate) fn refresh(&self) -> (Vec<KnownShell>, bool) {
        let _one = self.listing.lock().unwrap_or_else(PoisonError::into_inner);
        self.list_holding_lock()
    }

    /// [`Self::refresh`], for a caller that holds `listing`.
    fn list_holding_lock(&self) -> (Vec<KnownShell>, bool) {
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
                .flat_map(|listing| &listing.shells)
                .filter(|known| known.id.starts_with("wsl:"));
            shells.extend(before.cloned());
        }
        *listed = Some(ShellListing {
            shells: shells.clone(),
            wsl_answered,
        });
        self.generation.fetch_add(1, Ordering::SeqCst);
        (shells, wsl_answered)
    }

    /// The shell listed under `id`. A miss lists once more: the shell may
    /// be new, or WSL may have been slow when it was last listed. An id
    /// that no shell could have is refused without listing, and a listing
    /// that finished while this one waited for its turn is used instead of
    /// running another, unless `id` is a distribution and WSL did not
    /// answer in it.
    pub(crate) fn find(&self, id: &str) -> Option<LocalShell> {
        let seen = self.generation.load(Ordering::SeqCst);
        if let Some(shell) = self.listed_shell(id) {
            return Some(shell);
        }
        if !could_be_shell(id) {
            return None;
        }
        self.find_after(id, seen)
    }

    /// The rest of [`Self::find`] after a miss in the listing `seen` counted.
    fn find_after(&self, id: &str, seen: u64) -> Option<LocalShell> {
        let _one = self.listing.lock().unwrap_or_else(PoisonError::into_inner);
        if self.generation.load(Ordering::SeqCst) != seen {
            let (shell, settled) = self.last_answer(id);
            if settled {
                return shell;
            }
        }
        let (shells, _) = self.list_holding_lock();
        shells
            .into_iter()
            .find(|known| known.id == id)
            .map(|known| known.shell)
    }

    fn listed_shell(&self, id: &str) -> Option<LocalShell> {
        self.last_answer(id).0
    }

    /// The shell the last listing has under `id`, and whether that listing
    /// settles it: a distribution missing from a listing in which WSL did
    /// not answer may still be there, and no listing settles nothing.
    fn last_answer(&self, id: &str) -> (Option<LocalShell>, bool) {
        let listed = self.listed.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(listing) = listed.as_ref() else {
            return (None, false);
        };
        let shell = listing
            .shells
            .iter()
            .find(|known| known.id == id)
            .map(|known| known.shell.clone());
        let settled = shell.is_some() || listing.wsl_answered || !id.starts_with("wsl:");
        (shell, settled)
    }
}

/// Whether `id` has the form of an id the backend gives a shell.
fn could_be_shell(id: &str) -> bool {
    matches!(id, "powershell" | "pwsh" | "cmd" | "git-bash")
        || id.strip_prefix("wsl:").is_some_and(|name| !name.is_empty())
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
