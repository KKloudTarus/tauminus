// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Condvar;

use tauminus_domain::SessionMeta;
use tokio::sync::mpsc;

use super::*;
use crate::SessionConfig;
use crate::ports::{OutputSink, SinkClosed, TerminalEvent, TerminalStream, Terminator};
use crate::vault::tests::{secret, vault};

/// Keeps recordings in memory, as written; a gate can hold the writer.
#[derive(Default)]
struct MemoryStore {
    written: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    finished: Arc<Mutex<HashMap<String, RecordingMeta>>>,
    sealed_to: Mutex<Vec<[u8; 32]>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    next: Mutex<u32>,
}

impl MemoryStore {
    fn hold(&self, held: bool) {
        let (lock_, cv) = &*self.gate;
        *lock(lock_) = held;
        cv.notify_all();
    }

    fn text(&self, id: &str) -> String {
        String::from_utf8(lock(&self.written).get(id).cloned().unwrap_or_default()).unwrap()
    }
}

struct MemorySink {
    id: String,
    written: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    finished: Arc<Mutex<HashMap<String, RecordingMeta>>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl RecordingSink for MemorySink {
    fn write(&mut self, lines: &[u8]) -> Result<(), String> {
        let (held, cv) = &*self.gate;
        let mut held = lock(held);
        while *held {
            held = cv.wait(held).unwrap();
        }
        lock(&self.written)
            .entry(self.id.clone())
            .or_default()
            .extend_from_slice(lines);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn finish(self: Box<Self>, meta: &RecordingMeta) -> Result<(), String> {
        lock(&self.finished).insert(self.id.clone(), meta.clone());
        Ok(())
    }
}

impl RecordingStore for MemoryStore {
    fn new_id(&self) -> String {
        let mut next = lock(&self.next);
        *next += 1;
        format!("r{next}")
    }

    fn create(
        &self,
        meta: &RecordingMeta,
        public: &[u8; 32],
    ) -> Result<Box<dyn RecordingSink>, String> {
        lock(&self.sealed_to).push(*public);
        Ok(Box::new(MemorySink {
            id: meta.id.clone(),
            written: Arc::clone(&self.written),
            finished: Arc::clone(&self.finished),
            gate: Arc::clone(&self.gate),
        }))
    }

    fn list(&self) -> Result<Vec<RecordingMeta>, String> {
        Ok(lock(&self.finished).values().cloned().collect())
    }

    fn read(
        &self,
        id: &str,
        _secret: &[u8; 32],
        each: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        each(&lock(&self.written).get(id).cloned().unwrap_or_default());
        Ok(true)
    }

    fn delete(&self, id: &str) -> Result<bool, String> {
        Ok(lock(&self.written).remove(id).is_some())
    }

    fn save_checkpoints(&self, _: &str, _: &[u8; 32], _: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn checkpoints(&self, _: &str, _: &[u8; 32]) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    fn save_bookmarks(&self, _: &str, _: &[u8; 32], _: &[Bookmark]) -> Result<(), String> {
        Ok(())
    }

    fn bookmarks(&self, _: &str, _: &[u8; 32]) -> Result<Vec<Bookmark>, String> {
        Ok(Vec::new())
    }
}

struct Quiet;

impl OutputSink for Quiet {
    fn send_output(&self, _: Vec<u8>) -> Result<(), SinkClosed> {
        Ok(())
    }

    fn send_exit(&self, _: Option<u32>) {}
}

struct Nothing;

impl Terminator for Nothing {
    fn terminate(&self) {}
}

/// A session whose output the test sends.
fn session(sessions: &SessionManager) -> (SessionId, mpsc::Sender<TerminalEvent>) {
    let (events_tx, events) = mpsc::channel(64);
    let (input, mut input_rx) = mpsc::channel(64);
    tokio::spawn(async move { while input_rx.recv().await.is_some() {} });
    let id = sessions.attach(
        sessions.reserve().unwrap(),
        TerminalStream {
            input,
            events,
            terminator: Box::new(Nothing),
        },
        Quiet,
        SessionMeta::local(),
    );
    (id, events_tx)
}

fn size() -> TerminalSize {
    TerminalSize::new(100, 30).unwrap()
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out");
}

#[tokio::test]
async fn a_recording_is_the_output_as_asciicast_until_stopped() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let store = Arc::new(MemoryStore::default());
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let recordings = Recordings::new(
        Arc::clone(&store) as Arc<dyn RecordingStore>,
        Arc::clone(&vault),
        Arc::clone(&sessions),
    );
    let (id, events) = session(&sessions);
    let meta = recordings
        .start(
            id,
            size(),
            "db \"prod\"".into(),
            Some("db".into()),
            Some("\u{1b}[Hscreen before".into()),
        )
        .await
        .unwrap();
    // A character cut between two reads is recorded whole.
    events
        .send(TerminalEvent::Output("ol\u{e9}".as_bytes()[..3].to_vec()))
        .await
        .unwrap();
    events
        .send(TerminalEvent::Output(vec![0xa9, b'\n']))
        .await
        .unwrap();
    wait_for(|| store.text(&meta.id).contains('é')).await;
    sessions
        .resize(id, TerminalSize::new(80, 24).unwrap())
        .await
        .unwrap();
    let stopped = recordings.stop(id).unwrap();
    assert_eq!(stopped.bytes, 5);
    wait_for(|| lock(&store.finished).contains_key(&meta.id)).await;
    let text = store.text(&meta.id);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with(r#"{"version":2,"width":100,"height":30,"#),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains(r#""title":"db \"prod\"""#));
    // The pump may join the two reads into one event: the text is whole
    // either way.
    let output: String = lines[1..]
        .iter()
        .filter_map(|l| l.split(r#", "o", ""#).nth(1))
        .map(|l| l.trim_end_matches("\"]"))
        .collect();
    // The screen it began from comes first.
    assert_eq!(output, "\\u001b[Hscreen beforeolé\\n");
    assert!(
        lines.last().unwrap().ends_with(r#", "r", "80x24"]"#),
        "{text}"
    );
    let done = lock(&store.finished)[&meta.id].clone();
    assert!(done.complete && !done.truncated);
    // Output after the stop is not recorded.
    events
        .send(TerminalEvent::Output(b"later".to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!store.text(&meta.id).contains("later"));
    assert_eq!(recordings.recording().len(), 0);
}

#[tokio::test]
async fn a_locked_vault_records_once_its_key_exists_but_plays_back_only_open() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    vault.lock().await;
    let store = Arc::new(MemoryStore::default());
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let recordings = Recordings::new(
        Arc::clone(&store) as Arc<dyn RecordingStore>,
        Arc::clone(&vault),
        Arc::clone(&sessions),
    );
    let (id, _events) = session(&sessions);
    // The key is made on first use, which needs the vault open.
    assert!(matches!(
        recordings.start(id, size(), "a".into(), None, None).await,
        Err(RecordingError::Vault(VaultError::Locked))
    ));
    vault.unlock(secret("correct horse")).await.unwrap();
    let first = recordings
        .start(id, size(), "a".into(), None, None)
        .await
        .unwrap();
    recordings.stop(id);
    vault.lock().await;
    let again = recordings
        .start(id, size(), "a".into(), None, None)
        .await
        .unwrap();
    // The file is made on the writer thread.
    wait_for(|| lock(&store.sealed_to).len() == 2).await;
    let sealed = lock(&store.sealed_to).clone();
    assert_eq!(sealed[0], sealed[1]);
    assert!(matches!(
        recordings.read(first.id.clone(), |_| true).await,
        Err(RecordingError::Vault(VaultError::Locked))
    ));
    recordings.stop(id);
    assert_ne!(first.id, again.id);
}

#[tokio::test]
async fn a_disk_that_falls_behind_stops_the_recording_not_the_session() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let store = Arc::new(MemoryStore::default());
    store.hold(true);
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let recordings = Recordings::new(
        Arc::clone(&store) as Arc<dyn RecordingStore>,
        Arc::clone(&vault),
        Arc::clone(&sessions),
    );
    let (id, _events) = session(&sessions);
    let meta = recordings
        .start(id, size(), "a".into(), None, None)
        .await
        .unwrap();
    let recorder = Arc::clone(lock(&recordings.active).get(&id).unwrap());
    // Far more lines than the queue holds, while the writer is stuck: each
    // call returns at once, as the pump needs.
    let begun = Instant::now();
    for _ in 0..(QUEUE * 3) {
        recorder.output(b"x");
    }
    assert!(begun.elapsed() < Duration::from_secs(1), "the tap waited");
    wait_for(|| {
        recordings
            .recording()
            .first()
            .is_some_and(|(_, m)| m.truncated)
    })
    .await;
    store.hold(false);
    wait_for(|| lock(&store.finished).contains_key(&meta.id)).await;
    let done = lock(&store.finished)[&meta.id].clone();
    assert!(done.truncated);
    assert!(done.bytes < (QUEUE * 3) as u64);
}

#[tokio::test]
async fn the_end_of_a_session_ends_its_recording() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let store = Arc::new(MemoryStore::default());
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let recordings = Recordings::new(
        Arc::clone(&store) as Arc<dyn RecordingStore>,
        Arc::clone(&vault),
        Arc::clone(&sessions),
    );
    let (id, events) = session(&sessions);
    let meta = recordings
        .start(id, size(), "a".into(), None, None)
        .await
        .unwrap();
    assert!(matches!(
        recordings.start(id, size(), "a".into(), None, None).await,
        Err(RecordingError::AlreadyRecording)
    ));
    events
        .send(TerminalEvent::Output(b"bye".to_vec()))
        .await
        .unwrap();
    events
        .send(TerminalEvent::Exited { code: Some(0) })
        .await
        .unwrap();
    wait_for(|| lock(&store.finished).contains_key(&meta.id)).await;
    assert!(store.text(&meta.id).contains("bye"));
    assert!(lock(&store.finished)[&meta.id].complete);
    wait_for(|| recordings.recording().is_empty()).await;
}

/// A sink that fails its writes after the first, as a full disk does.
struct FailingSink {
    writes: usize,
    finished: Arc<Mutex<Option<RecordingMeta>>>,
}

impl RecordingSink for FailingSink {
    fn write(&mut self, _lines: &[u8]) -> Result<(), String> {
        self.writes += 1;
        if self.writes > 1 {
            Err("no space left".into())
        } else {
            Ok(())
        }
    }

    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn finish(self: Box<Self>, meta: &RecordingMeta) -> Result<(), String> {
        *lock(&self.finished) = Some(meta.clone());
        Ok(())
    }
}

#[tokio::test]
async fn a_disk_that_fails_stops_the_recording_and_says_it_was_cut_short() {
    let finished = Arc::new(Mutex::new(None));
    let recorder = Recorder::start(
        {
            let finished = Arc::clone(&finished);
            Box::new(move || {
                Ok(Box::new(FailingSink {
                    writes: 0,
                    finished,
                }) as Box<dyn RecordingSink>)
            })
        },
        RecordingMeta {
            id: "r1".into(),
            title: "a".into(),
            host: None,
            started_ms: 0,
            duration_ms: 0,
            bytes: 0,
            truncated: false,
            complete: false,
        },
        size(),
        None,
    )
    .unwrap();
    recorder.output(b"lost");
    wait_for(|| recorder.failed.load(Ordering::Acquire)).await;
    // The next output finds the recording stopped.
    recorder.output(b"more");
    recorder.end();
    wait_for(|| lock(&finished).is_some()).await;
    let meta = lock(&finished).clone().unwrap();
    assert!(meta.truncated, "{meta:?}");
}

#[tokio::test]
async fn a_recording_whose_session_ended_can_be_deleted() {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let store = Arc::new(MemoryStore::default());
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let recordings = Recordings::new(
        Arc::clone(&store) as Arc<dyn RecordingStore>,
        Arc::clone(&vault),
        Arc::clone(&sessions),
    );
    let (id, events) = session(&sessions);
    let meta = recordings
        .start(id, size(), "a".into(), None, None)
        .await
        .unwrap();
    events
        .send(TerminalEvent::Output(b"bye".to_vec()))
        .await
        .unwrap();
    events
        .send(TerminalEvent::Exited { code: Some(0) })
        .await
        .unwrap();
    wait_for(|| lock(&store.finished).contains_key(&meta.id)).await;
    // Nothing asked which panes record: delete must not think it still does.
    recordings.delete(meta.id.clone()).await.unwrap();
    // And a tap put on a session that ended is refused, not left unended.
    assert!(sessions.set_tap(id, None).is_err());
}
