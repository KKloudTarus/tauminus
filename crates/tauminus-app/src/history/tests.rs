// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex as StdMutex;

use super::*;
use crate::vault::tests::{age, secret, vault};

/// The database and pending file in memory. A sealed entry keeps the key it
/// was sealed to, so opening checks that the right key was used.
#[derive(Default)]
struct Memory {
    rows: Arc<StdMutex<Vec<HistoryEntry>>>,
    pending: StdMutex<Vec<([u8; 32], HistoryEntry)>>,
    opened: StdMutex<u32>,
    /// Every try to open, failed or not.
    tries: std::sync::atomic::AtomicU32,
    /// The database will not open.
    broken: std::sync::atomic::AtomicBool,
}

struct Rows(Arc<StdMutex<Vec<HistoryEntry>>>);

impl HistoryStore for Rows {
    fn insert(
        &self,
        entries: &[HistoryEntry],
        open: &dyn Fn() -> bool,
    ) -> Result<bool, HistoryError> {
        if !open() {
            return Ok(false);
        }
        self.0.lock().unwrap().extend_from_slice(entries);
        Ok(true)
    }

    fn search(&self, query: &HistoryQuery) -> Result<Vec<HistoryHit>, HistoryError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, e)| e.command.contains(query.text.trim()))
            .map(|(id, e)| HistoryHit {
                id: i64::try_from(id).unwrap(),
                host: e.host.clone(),
                label: e.label.clone(),
                at_ms: e.at_ms,
                exit: e.exit,
                command: e.command.clone(),
                snippet: String::new(),
            })
            .collect())
    }

    fn output(&self, id: i64) -> Result<Option<String>, HistoryError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(usize::try_from(id).unwrap())
            .map(|e| e.output.clone()))
    }

    fn prune(
        &self,
        retention: Retention,
        _now_ms: i64,
        _open: &dyn Fn() -> bool,
    ) -> Result<u64, HistoryError> {
        let mut rows = self.0.lock().unwrap();
        let mut removed = 0;
        while rows.iter().map(HistoryEntry::size).sum::<u64>() > retention.max_bytes {
            rows.remove(0);
            removed += 1;
        }
        Ok(removed)
    }

    fn clear(&self, host: Option<&str>) -> Result<u64, HistoryError> {
        let mut rows = self.0.lock().unwrap();
        let before = rows.len();
        rows.retain(|e| host.is_some_and(|h| e.host.as_deref() != Some(h)));
        Ok((before - rows.len()) as u64)
    }
}

impl HistoryBackend for Arc<Memory> {
    fn open(&self, _keys: &HistoryKeys) -> Result<Arc<dyn HistoryStore>, HistoryError> {
        self.tries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.broken.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(HistoryError::Store("disk on fire".into()));
        }
        *self.opened.lock().unwrap() += 1;
        Ok(Arc::new(Rows(Arc::clone(&self.rows))))
    }

    fn seal(&self, public: &[u8; 32], entry: &HistoryEntry) -> Result<bool, HistoryError> {
        self.pending.lock().unwrap().push((*public, entry.clone()));
        Ok(true)
    }

    fn import(
        &self,
        keys: &HistoryKeys,
        store: &mut dyn FnMut(&[HistoryEntry]) -> Result<(), HistoryError>,
    ) -> Result<usize, HistoryError> {
        let mut pending = self.pending.lock().unwrap();
        let opened = pending
            .iter()
            .filter(|(public, _)| *public == keys.public)
            .map(|(_, e)| e.clone())
            .collect::<Vec<_>>();
        let unreadable = pending.len() - opened.len();
        if !opened.is_empty() {
            store(&opened)?;
        }
        pending.clear();
        Ok(unreadable)
    }

    fn now_ms(&self) -> i64 {
        0
    }
}

fn entry(command: &str) -> HistoryEntry {
    HistoryEntry {
        host: Some("web".into()),
        label: "web".into(),
        at_ms: 1,
        command: command.into(),
        output: "out".into(),
        exit: Some(0),
    }
}

async fn history(retention: Retention) -> (Arc<Memory>, Arc<Vault>, Arc<History>) {
    let (_, vault) = vault();
    let memory = Arc::new(Memory::default());
    let history = History::new(Arc::new(Arc::clone(&memory)), Arc::clone(&vault), retention).await;
    (memory, vault, history)
}

fn found(hits: &[HistoryHit]) -> Vec<&str> {
    hits.iter().map(|h| h.command.as_str()).collect()
}

fn query(text: &str) -> HistoryQuery {
    HistoryQuery {
        text: text.into(),
        limit: 50,
        ..HistoryQuery::default()
    }
}

#[tokio::test]
async fn without_a_vault_nothing_is_kept() {
    let (memory, _, history) = history(Retention::default()).await;
    assert_eq!(history.record(entry("ls")).await, Ok(Recorded::Dropped));
    assert!(memory.pending.lock().unwrap().is_empty());
    assert_eq!(history.search(query("")).await, Err(HistoryError::Locked));
}

#[tokio::test]
async fn commands_go_to_the_database_while_open_and_are_sealed_while_locked() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    assert_eq!(history.record(entry("first")).await, Ok(Recorded::Stored));
    vault.lock().await;
    assert_eq!(history.record(entry("second")).await, Ok(Recorded::Sealed));
    assert_eq!(history.record(entry("third")).await, Ok(Recorded::Sealed));
    assert_eq!(memory.rows.lock().unwrap().len(), 1);
    assert_eq!(history.search(query("")).await, Err(HistoryError::Locked));

    // Unlocking imports what was sealed, in order, and empties the file.
    vault.unlock(secret("correct horse")).await.unwrap();
    let hits = history.search(query("")).await.unwrap();
    assert_eq!(found(&hits), ["third", "second", "first"]);
    assert!(memory.pending.lock().unwrap().is_empty());
    assert_eq!(
        history.output(hits[0].id).await.unwrap().as_deref(),
        Some("out")
    );
}

#[tokio::test]
async fn the_database_opens_once_per_unlock() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    for n in 0..5 {
        history.record(entry(&format!("cmd{n}"))).await.unwrap();
    }
    assert_eq!(*memory.opened.lock().unwrap(), 1);
}

#[tokio::test]
async fn empty_commands_are_not_kept_and_queries_are_checked() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    assert_eq!(history.record(entry("   ")).await, Ok(Recorded::Dropped));
    assert!(memory.rows.lock().unwrap().is_empty());
    let mut bad = query("x");
    bad.limit = 0;
    assert!(matches!(
        history.search(bad).await,
        Err(HistoryError::Invalid(_))
    ));
}

#[tokio::test]
async fn opening_applies_the_size_cap() {
    let retention = Retention {
        max_bytes: 20,
        max_days: 0,
    };
    let (memory, vault, history) = history(retention).await;
    vault.create(secret("correct horse")).await.unwrap();
    // The history's keys are made the first time it opens.
    history.search(query("")).await.unwrap();
    vault.lock().await;
    for n in 0..4 {
        history.record(entry(&format!("cmd{n}"))).await.unwrap();
    }
    vault.unlock(secret("correct horse")).await.unwrap();
    // The prune runs in the background, after the one the first open
    // started if that one is still going.
    history.search(query("")).await.unwrap();
    for _ in 0..500 {
        if memory.rows.lock().unwrap().len() <= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // Each entry is "cmdN" + "out" + "web": 10 bytes; two fit.
    assert_eq!(
        found(&history.search(query("")).await.unwrap()),
        ["cmd3", "cmd2"]
    );
    assert_eq!(memory.rows.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn clearing_forgets_a_host() {
    let (_, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    history.record(entry("on web")).await.unwrap();
    let mut local = entry("on local");
    local.host = None;
    history.record(local).await.unwrap();
    assert_eq!(history.clear(Some("web".into())).await, Ok(1));
    assert_eq!(
        found(&history.search(query("")).await.unwrap()),
        ["on local"]
    );
}

#[tokio::test]
async fn nothing_reaches_the_database_after_a_lock() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    history.record(entry("open")).await.unwrap();
    // No yield: the follower has not run yet.
    vault.lock().await;
    assert_eq!(history.record(entry("locked")).await, Ok(Recorded::Sealed));
    assert_eq!(memory.rows.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_database_that_will_not_open_still_keeps_commands_sealed() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    history.search(query("")).await.unwrap();
    vault.lock().await;
    memory
        .broken
        .store(true, std::sync::atomic::Ordering::Relaxed);
    vault.unlock(secret("correct horse")).await.unwrap();
    assert_eq!(history.record(entry("kept")).await, Ok(Recorded::Sealed));
    assert_eq!(memory.pending.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_vault_past_its_idle_time_gives_no_history() {
    let (_, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    history.record(entry("before")).await.unwrap();
    age(&vault).await;
    // The tick has not run; the search still sees the vault as locked.
    assert_eq!(history.search(query("")).await, Err(HistoryError::Locked));
    assert_eq!(history.record(entry("after")).await, Ok(Recorded::Sealed));
}

#[tokio::test]
async fn a_lock_the_follower_missed_still_closes_the_database() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    history.search(query("")).await.unwrap();
    assert_eq!(*memory.opened.lock().unwrap(), 1);
    // No yield between them: the follower sees neither.
    vault.lock().await;
    vault.unlock(secret("correct horse")).await.unwrap();
    history.search(query("")).await.unwrap();
    assert_eq!(
        *memory.opened.lock().unwrap(),
        2,
        "the database of the earlier session was reused"
    );
}

#[tokio::test]
async fn a_database_that_failed_to_open_is_not_retried_until_the_next_unlock() {
    let (memory, vault, history) = history(Retention::default()).await;
    vault.create(secret("correct horse")).await.unwrap();
    history.search(query("")).await.unwrap();
    vault.lock().await;
    memory
        .broken
        .store(true, std::sync::atomic::Ordering::Relaxed);
    vault.unlock(secret("correct horse")).await.unwrap();
    let tries = || memory.tries.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(history.record(entry("kept")).await, Ok(Recorded::Sealed));
    let after_one = tries();
    for _ in 0..3 {
        assert_eq!(history.record(entry("kept")).await, Ok(Recorded::Sealed));
    }
    assert_eq!(tries(), after_one, "opening was retried on every record");
    // The next unlock tries again.
    memory
        .broken
        .store(false, std::sync::atomic::Ordering::Relaxed);
    vault.lock().await;
    vault.unlock(secret("correct horse")).await.unwrap();
    assert_eq!(history.record(entry("stored")).await, Ok(Recorded::Stored));
}
