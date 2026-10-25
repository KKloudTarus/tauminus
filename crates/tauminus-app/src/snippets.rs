// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::BTreeMap;
use std::sync::Arc;

use tauminus_domain::{HostsError, Library, SnippetError, SnippetId};
use thiserror::Error;
use tokio::sync::{Mutex, watch};

use crate::hosts::Reading;
use crate::ports::{SnippetStore, StoreError};

#[derive(Debug, Error)]
pub enum SnippetBookError {
    #[error(transparent)]
    Snippet(#[from] SnippetError),
    #[error(transparent)]
    Id(#[from] HostsError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A startup snippet that cannot run, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub id: SnippetId,
    pub reason: String,
}

/// The saved snippets. As with [`crate::HostBook`], every change is saved
/// before it becomes visible, and changes are serialized; each costs O(S)
/// to copy and save, S ≤ 2000.
pub struct SnippetBook {
    store: Arc<dyn SnippetStore>,
    library: Mutex<Library>,
    revision: watch::Sender<u64>,
}

impl std::fmt::Debug for SnippetBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnippetBook").finish_non_exhaustive()
    }
}

impl SnippetBook {
    pub async fn open(store: Arc<dyn SnippetStore>) -> Result<Self, StoreError> {
        let library = store.load().await?;
        Ok(Self {
            store,
            library: Mutex::new(library),
            revision: watch::Sender::new(0),
        })
    }

    pub async fn snapshot(&self) -> Library {
        self.library.lock().await.clone()
    }

    pub fn new_id(&self) -> Result<SnippetId, HostsError> {
        SnippetId::parse(&self.store.new_id())
    }

    /// Applies `change` to a copy, saves it, then publishes it.
    pub async fn edit(
        &self,
        change: impl FnOnce(&mut Library) -> Result<(), SnippetError>,
    ) -> Result<Library, SnippetBookError> {
        Ok(self.edit_both(change).await?.1)
    }

    /// As [`SnippetBook::edit`], and the library just before the change.
    pub async fn edit_both(
        &self,
        change: impl FnOnce(&mut Library) -> Result<(), SnippetError>,
    ) -> Result<(Library, Library), SnippetBookError> {
        let mut current = self.library.lock().await;
        let mut next = current.clone();
        change(&mut next)?;
        match self.store.save(&next).await {
            Ok(()) => {}
            Err(StoreError::Conflict) => {
                // Take the other window's library, so the next try builds on it.
                *current = self.store.load().await?;
                self.revision.send_modify(|r| *r += 1);
                return Err(StoreError::Conflict.into());
            }
            Err(e) => return Err(e.into()),
        }
        let before = std::mem::replace(&mut *current, next.clone());
        self.revision.send_modify(|r| *r += 1);
        Ok((before, next))
    }

    /// Replaces the library with `next` if it is still `expected`, and
    /// saves it, as [`crate::HostBook::replace_if`] does.
    pub async fn replace_if(
        &self,
        expected: &Library,
        next: Library,
    ) -> Result<bool, SnippetBookError> {
        let mut current = self.library.lock().await;
        if *current != *expected {
            return Ok(false);
        }
        if *current != next {
            self.store.save(&next).await?;
            *current = next;
            self.revision.send_modify(|r| *r += 1);
        }
        Ok(true)
    }

    /// As [`SnippetBook::replace_if`], and only while `hosts` still holds
    /// `hosts_now`: a sync saves the snippets after the hosts, and a host
    /// edited in between may list a snippet the merge changed without its
    /// note. Takes the hosts' lock, then the snippets', as
    /// [`crate::trust::Trust::check`] does.
    pub async fn replace_if_hosts(
        &self,
        expected: &Library,
        next: Library,
        hosts: &crate::HostBook,
        hosts_now: &tauminus_domain::Inventory,
    ) -> Result<bool, SnippetBookError> {
        let inventory = hosts.reading().await;
        if *inventory != *hosts_now {
            return Ok(false);
        }
        self.replace_if(expected, next).await
    }

    /// What to type to run snippet `id` with `values`. O(n + output).
    pub async fn command(
        &self,
        id: &SnippetId,
        values: &BTreeMap<String, String>,
    ) -> Result<String, SnippetError> {
        let library = self.library.lock().await;
        let snippet = library
            .get(id)
            .ok_or_else(|| SnippetError::NoSuch(id.clone()))?;
        snippet.command(values)
    }

    /// What to type for a host's startup snippets; see [`startup_text`].
    pub async fn startup(&self, ids: &[SnippetId]) -> (String, Vec<Skipped>) {
        startup_text(&*self.library.lock().await, ids)
    }

    /// The library as it is now, without a copy, read-only. Holds its lock
    /// until dropped.
    pub async fn reading(&self) -> Reading<'_, Library> {
        Reading::new(self.library.lock().await)
    }

    /// Bumped on every change to the library, a sync's included.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }
}

/// What to type for a host's startup snippets, in order, with their
/// defaults: one string for those that can run, and the rest with the
/// reason. A snippet deleted since, or with a placeholder that has no
/// default, is skipped. O(Σ n).
pub fn startup_text(library: &Library, ids: &[SnippetId]) -> (String, Vec<Skipped>) {
    let mut text = String::new();
    let mut skipped = Vec::new();
    for id in ids {
        let result = library
            .get(id)
            .ok_or_else(|| SnippetError::NoSuch(id.clone()))
            .and_then(|s| s.command(&BTreeMap::new()));
        match result {
            Ok(command) => text.push_str(&command),
            Err(error) => skipped.push(Skipped {
                id: id.clone(),
                reason: error.to_string(),
            }),
        }
    }
    (text, skipped)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use tauminus_domain::{Label, Snippet, SnippetError, SnippetId};

    use super::{SnippetBook, SnippetBookError};
    use crate::fakes::FakeSnippetStore;
    use crate::ports::StoreError;

    fn snippet(id: &str, body: &str) -> Snippet {
        Snippet::new(
            SnippetId::parse(id).unwrap(),
            Label::parse(id).unwrap(),
            None,
            body.to_owned(),
        )
        .unwrap()
    }

    fn id(text: &str) -> SnippetId {
        SnippetId::parse(text).unwrap()
    }

    #[tokio::test]
    async fn a_change_is_saved_before_it_shows() {
        let store = Arc::new(FakeSnippetStore::default());
        let book = SnippetBook::open(store.clone()).await.unwrap();
        book.edit(|l| l.put(snippet("a", "uptime"))).await.unwrap();
        assert_eq!(store.saved().unwrap().snippets().count(), 1);
        store.fail_next();
        let failed = book.edit(|l| l.put(snippet("b", "date"))).await;
        assert!(matches!(
            failed,
            Err(SnippetBookError::Store(StoreError::Io(_)))
        ));
        assert!(book.snapshot().await.get(&id("b")).is_none());
    }

    #[tokio::test]
    async fn a_command_fills_values_and_ends_lines_with_enter() {
        let book = SnippetBook::open(Arc::new(FakeSnippetStore::default()))
            .await
            .unwrap();
        book.edit(|l| l.put(snippet("logs", "tail -n {{n:50}} {{file}}")))
            .await
            .unwrap();
        let values = BTreeMap::from([("file".to_owned(), "/var/log/syslog".to_owned())]);
        assert_eq!(
            book.command(&id("logs"), &values).await.unwrap(),
            "tail -n 50 /var/log/syslog\r"
        );
        assert_eq!(
            book.command(&id("gone"), &values).await,
            Err(SnippetError::NoSuch(id("gone")))
        );
    }

    #[tokio::test]
    async fn startup_runs_what_it_can_and_says_what_it_skipped() {
        let book = SnippetBook::open(Arc::new(FakeSnippetStore::default()))
            .await
            .unwrap();
        book.edit(|l| {
            l.put(snippet("tmux", "tmux new -A -s {{name:main}}"))?;
            l.put(snippet("cd", "cd {{dir}}"))?;
            l.put(snippet("env", "export TERM=xterm-256color\nclear"))
        })
        .await
        .unwrap();
        let (text, skipped) = book
            .startup(&[id("tmux"), id("cd"), id("gone"), id("env")])
            .await;
        assert_eq!(
            text,
            "tmux new -A -s main\rexport TERM=xterm-256color\rclear\r"
        );
        let ids: Vec<_> = skipped.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["cd", "gone"]);
    }
}
