// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;

use tauminus_domain::LocalShell;
use tokio::sync::mpsc::error::TrySendError;

use super::*;
use crate::LocalShells;
use crate::ports::{OutputTap, PtyLauncher, SinkClosed};

/// The far end of a fake stream, held by the test.
struct Remote {
    events: mpsc::Sender<TerminalEvent>,
    input: mpsc::Receiver<TerminalInput>,
    terminated: Arc<AtomicUsize>,
    killed: Arc<AtomicUsize>,
    probed: Arc<AtomicUsize>,
}

impl Remote {
    fn probes(&self) -> usize {
        self.probed.load(Ordering::Relaxed)
    }

    fn terminations(&self) -> usize {
        self.terminated.load(Ordering::Relaxed)
    }

    fn kills(&self) -> usize {
        self.killed.load(Ordering::Relaxed)
    }
}

struct CountingTerminator {
    terminated: Arc<AtomicUsize>,
    killed: Arc<AtomicUsize>,
    probed: Arc<AtomicUsize>,
}

impl Terminator for CountingTerminator {
    fn terminate(&self) {
        self.terminated.fetch_add(1, Ordering::Relaxed);
    }

    fn kill(&self) {
        self.killed.fetch_add(1, Ordering::Relaxed);
    }

    fn probe(&self) {
        self.probed.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Default)]
struct FakeLauncher {
    remotes: Arc<Mutex<VecDeque<Remote>>>,
    fail_next: Arc<AtomicBool>,
    event_capacity: Arc<AtomicUsize>,
    launches: Arc<AtomicUsize>,
}

impl FakeLauncher {
    fn with_event_capacity(capacity: usize) -> Self {
        let launcher = Self::default();
        launcher.event_capacity.store(capacity, Ordering::Relaxed);
        launcher
    }

    fn take_remote(&self) -> Remote {
        lock(&self.remotes)
            .pop_front()
            .expect("a stream was launched")
    }
}

impl PtyLauncher for FakeLauncher {
    fn launch(
        &self,
        _shell: &LocalShell,
        _size: TerminalSize,
    ) -> Result<TerminalStream, LaunchError> {
        self.launches.fetch_add(1, Ordering::Relaxed);
        if self.fail_next.swap(false, Ordering::Relaxed) {
            return Err(LaunchError::Spawn {
                program: "fake-shell".into(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            });
        }
        let capacity = match self.event_capacity.load(Ordering::Relaxed) {
            0 => 64,
            n => n,
        };
        let (events_tx, events_rx) = mpsc::channel(capacity);
        let (input_tx, input_rx) = mpsc::channel(64);
        let terminated = Arc::new(AtomicUsize::new(0));
        let killed = Arc::new(AtomicUsize::new(0));
        let probed = Arc::new(AtomicUsize::new(0));
        lock(&self.remotes).push_back(Remote {
            events: events_tx,
            input: input_rx,
            terminated: Arc::clone(&terminated),
            killed: Arc::clone(&killed),
            probed: Arc::clone(&probed),
        });
        Ok(TerminalStream {
            input: input_tx,
            events: events_rx,
            terminator: Box::new(CountingTerminator {
                terminated,
                killed,
                probed,
            }),
        })
    }
}

#[derive(Clone, Default)]
struct RecordingSink {
    batches: Arc<Mutex<Vec<Vec<u8>>>>,
    exits: Arc<Mutex<Vec<Option<u32>>>>,
    lost: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl RecordingSink {
    fn batches(&self) -> Vec<Vec<u8>> {
        lock(&self.batches).clone()
    }

    fn output(&self) -> Vec<u8> {
        self.batches().concat()
    }

    fn exits(&self) -> Vec<Option<u32>> {
        lock(&self.exits).clone()
    }
}

impl OutputSink for RecordingSink {
    fn send_output(&self, batch: Vec<u8>) -> Result<(), SinkClosed> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(SinkClosed);
        }
        lock(&self.batches).push(batch);
        Ok(())
    }

    fn send_exit(&self, code: Option<u32>) {
        lock(&self.exits).push(code);
    }

    fn send_lost(&self) {
        self.lost.fetch_add(1, Ordering::Relaxed);
    }
}

fn config() -> SessionConfig {
    SessionConfig {
        max_sessions: 8,
        flush_bytes: 1024,
        flush_interval: Duration::from_millis(8),
        watermarks: Watermarks::default(),
        close_grace: Duration::from_secs(2),
    }
}

struct Harness {
    launcher: FakeLauncher,
    sessions: Arc<SessionManager>,
    shells: LocalShells,
    sink: RecordingSink,
}

impl Harness {
    fn new(config: SessionConfig) -> Self {
        Self::with_launcher(FakeLauncher::default(), config)
    }

    fn with_launcher(launcher: FakeLauncher, config: SessionConfig) -> Self {
        let sessions = Arc::new(SessionManager::new(config));
        let shells = LocalShells::new(Arc::new(launcher.clone()), Arc::clone(&sessions));
        Self {
            launcher,
            sessions,
            shells,
            sink: RecordingSink::default(),
        }
    }

    fn open(&self) -> Result<SessionId, SessionError> {
        self.shells.open(
            &LocalShell::default(),
            TerminalSize::default(),
            self.sink.clone(),
        )
    }

    fn open_ok(&self) -> (SessionId, Remote) {
        let id = self.open().expect("session opens");
        (id, self.launcher.take_remote())
    }
}

/// Lets spawned tasks run. With the paused test clock, a sleep advances time
/// only once every task is idle, so this is deterministic.
async fn settle() {
    tokio::time::sleep(Duration::from_micros(1)).await;
}

async fn output(remote: &Remote, bytes: &[u8]) {
    remote
        .events
        .send(TerminalEvent::Output(bytes.to_vec()))
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn open_assigns_unique_ids() {
    let h = Harness::new(config());
    let (a, _ra) = h.open_ok();
    let (b, _rb) = h.open_ok();
    assert_ne!(a, b);
    assert_eq!(h.sessions.open_count(), 2);
    assert_eq!(h.sessions.meta(a).unwrap(), SessionMeta::local());
}

#[tokio::test(start_paused = true)]
async fn echo_after_quiet_period_is_sent_without_waiting() {
    let h = Harness::new(config());
    let (_, remote) = h.open_ok();

    output(&remote, b"a").await;
    tokio::task::yield_now().await;
    assert_eq!(
        h.sink.batches(),
        vec![b"a".to_vec()],
        "first output is immediate"
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    output(&remote, b"b").await;
    tokio::task::yield_now().await;
    assert_eq!(
        h.sink.output(),
        b"ab",
        "echo after a quiet period is immediate"
    );
}

#[tokio::test(start_paused = true)]
async fn sustained_output_is_coalesced_until_the_interval() {
    let h = Harness::new(config());
    let (_, remote) = h.open_ok();

    output(&remote, b"1").await;
    tokio::task::yield_now().await;
    // Within the interval of the last flush: held and coalesced.
    for chunk in [&b"2"[..], b"3", b"4"] {
        output(&remote, chunk).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(h.sink.batches(), vec![b"1".to_vec()]);

    tokio::time::sleep(Duration::from_millis(9)).await;
    assert_eq!(h.sink.batches(), vec![b"1".to_vec(), b"234".to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn queued_reads_become_one_batch() {
    let h = Harness::new(config());
    let (_, remote) = h.open_ok();
    // Queue several reads before the pump runs; it drains them in one pass.
    for chunk in [&b"he"[..], b"ll", b"o"] {
        remote
            .events
            .try_send(TerminalEvent::Output(chunk.to_vec()))
            .unwrap();
    }
    settle().await;
    assert_eq!(h.sink.batches(), vec![b"hello".to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn output_reaching_flush_size_is_sent_without_waiting() {
    let h = Harness::new(SessionConfig {
        flush_bytes: 4,
        ..config()
    });
    let (_, remote) = h.open_ok();
    output(&remote, b"x").await;
    tokio::task::yield_now().await;
    output(&remote, b"ab").await;
    output(&remote, b"cd").await;
    tokio::task::yield_now().await;
    assert_eq!(h.sink.batches(), vec![b"x".to_vec(), b"abcd".to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn output_order_is_preserved_across_batches() {
    let h = Harness::new(SessionConfig {
        flush_bytes: 5,
        ..config()
    });
    let (_, remote) = h.open_ok();
    let expected: Vec<u8> = (0..=255).cycle().take(1000).collect();
    for chunk in expected.chunks(7) {
        output(&remote, chunk).await;
    }
    remote
        .events
        .send(TerminalEvent::Exited { code: Some(0) })
        .await
        .unwrap();
    settle().await;
    assert_eq!(h.sink.output(), expected);
}

#[tokio::test(start_paused = true)]
async fn exit_flushes_pending_output_then_reports_code() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    output(&remote, b"by").await;
    output(&remote, b"e").await;
    remote
        .events
        .send(TerminalEvent::Exited { code: Some(3) })
        .await
        .unwrap();
    settle().await;

    assert_eq!(h.sink.output(), b"bye");
    assert_eq!(h.sink.exits(), vec![Some(3)]);
    assert_eq!(h.sessions.open_count(), 0);
    assert!(matches!(
        h.sessions.write(id, b"x".to_vec()).await,
        Err(SessionError::NotFound(found)) if found == id
    ));
}

#[tokio::test(start_paused = true)]
async fn stream_dropping_without_exit_reports_unknown_code() {
    let h = Harness::new(config());
    let (_, remote) = h.open_ok();
    drop(remote);
    settle().await;
    assert_eq!(h.sink.exits(), vec![None]);
    assert_eq!(h.sessions.open_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn write_and_resize_reach_stream_in_order() {
    let h = Harness::new(config());
    let (id, mut remote) = h.open_ok();
    let size = TerminalSize::new(132, 40).unwrap();
    h.sessions.write(id, b"ls\r".to_vec()).await.unwrap();
    h.sessions.resize(id, size).await.unwrap();
    h.sessions.write(id, b"\x03".to_vec()).await.unwrap();

    assert_eq!(
        remote.input.recv().await,
        Some(TerminalInput::Data(b"ls\r".to_vec()))
    );
    assert_eq!(remote.input.recv().await, Some(TerminalInput::Resize(size)));
    assert_eq!(
        remote.input.recv().await,
        Some(TerminalInput::Data(b"\x03".to_vec()))
    );
}

#[tokio::test(start_paused = true)]
async fn operations_on_unknown_session_return_not_found() {
    let h = Harness::new(config());
    let ghost = SessionId::new(999);
    assert!(matches!(
        h.sessions.write(ghost, vec![1]).await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        h.sessions.resize(ghost, TerminalSize::default()).await,
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        h.sessions.acknowledge(ghost, 1),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        h.sessions.close(ghost),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        h.sessions.meta(ghost),
        Err(SessionError::NotFound(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn session_limit_is_enforced_until_a_session_ends() {
    let h = Harness::new(SessionConfig {
        max_sessions: 1,
        ..config()
    });
    let (_, remote) = h.open_ok();
    assert!(matches!(
        h.open(),
        Err(SessionError::LimitReached { max: 1 })
    ));
    assert_eq!(
        h.launcher.launches.load(Ordering::Relaxed),
        1,
        "a full table must not start a process"
    );

    remote
        .events
        .send(TerminalEvent::Exited { code: Some(0) })
        .await
        .unwrap();
    settle().await;
    h.open_ok();
}

#[tokio::test(start_paused = true)]
async fn failed_launch_reports_error_and_frees_the_slot() {
    let h = Harness::new(SessionConfig {
        max_sessions: 1,
        ..config()
    });
    h.launcher.fail_next.store(true, Ordering::Relaxed);
    assert!(matches!(
        h.open(),
        Err(SessionError::Launch(LaunchError::Spawn { .. }))
    ));
    h.open_ok();
}

#[tokio::test(start_paused = true)]
async fn unacknowledged_output_pauses_reading_until_view_catches_up() {
    // 4-byte batches; pause once more than 8 bytes are in flight.
    let h = Harness::with_launcher(
        FakeLauncher::with_event_capacity(2),
        SessionConfig {
            flush_bytes: 4,
            watermarks: Watermarks::new(8, 4).unwrap(),
            ..config()
        },
    );
    let (id, remote) = h.open_ok();

    for _ in 0..3 {
        output(&remote, &[b'x'; 4]).await;
        settle().await;
    }
    assert_eq!(
        h.sink.batches().len(),
        3,
        "12 bytes in flight > high watermark of 8"
    );

    remote
        .events
        .try_send(TerminalEvent::Output(vec![b'y'; 4]))
        .unwrap();
    remote
        .events
        .try_send(TerminalEvent::Output(vec![b'y'; 4]))
        .unwrap();
    assert!(matches!(
        remote.events.try_send(TerminalEvent::Output(vec![b'z'; 4])),
        Err(TrySendError::Full(_))
    ));
    settle().await;
    assert_eq!(h.sink.batches().len(), 3, "nothing delivered while paused");

    // 12 - 9 = 3 bytes in flight, below the low watermark of 4.
    h.sessions.acknowledge(id, 9).unwrap();
    settle().await;
    assert_eq!(
        h.sink.output().len(),
        20,
        "queued output drains after resume"
    );
}

#[tokio::test(start_paused = true)]
async fn closing_a_paused_session_terminates_it_and_frees_the_slot() {
    let h = Harness::with_launcher(
        FakeLauncher::with_event_capacity(2),
        SessionConfig {
            max_sessions: 1,
            flush_bytes: 4,
            watermarks: Watermarks::new(8, 4).unwrap(),
            ..config()
        },
    );
    let (id, remote) = h.open_ok();
    for _ in 0..3 {
        output(&remote, &[b'x'; 4]).await;
        settle().await;
    }
    remote
        .events
        .try_send(TerminalEvent::Output(vec![b'y'; 4]))
        .unwrap();
    remote
        .events
        .try_send(TerminalEvent::Output(vec![b'y'; 4]))
        .unwrap();
    settle().await;

    h.sessions.close(id).unwrap();
    settle().await;
    assert_eq!(remote.terminations(), 1, "close reaches a paused stream");

    // The closing pump drains the stream even though nothing acknowledges.
    remote
        .events
        .send(TerminalEvent::Output(vec![b'z'; 4]))
        .await
        .unwrap();
    remote
        .events
        .send(TerminalEvent::Exited { code: None })
        .await
        .unwrap();
    settle().await;
    assert_eq!(h.sink.exits(), vec![None]);
    h.open_ok();
}

#[tokio::test(start_paused = true)]
async fn closed_view_terminates_the_stream() {
    let h = Harness::new(SessionConfig {
        flush_bytes: 1,
        ..config()
    });
    h.sink.closed.store(true, Ordering::Relaxed);
    let (_, remote) = h.open_ok();
    output(&remote, b"x").await;
    settle().await;
    assert_eq!(remote.terminations(), 1);
    assert_eq!(h.sessions.open_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn close_terminates_and_still_delivers_the_exit() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    h.sessions.close(id).unwrap();
    settle().await;
    assert_eq!(remote.terminations(), 1);
    assert!(matches!(
        h.sessions.close(id),
        Err(SessionError::NotFound(_))
    ));

    remote
        .events
        .send(TerminalEvent::Exited { code: None })
        .await
        .unwrap();
    settle().await;
    assert_eq!(h.sink.exits(), vec![None]);
}

#[tokio::test(start_paused = true)]
async fn close_all_terminates_every_session() {
    let h = Harness::new(config());
    let (_, a) = h.open_ok();
    let (_, b) = h.open_ok();
    assert_eq!(h.sessions.close_all(), 2);
    settle().await;
    assert_eq!((a.terminations(), b.terminations()), (1, 1));
    assert_eq!(h.sessions.open_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn closed_stream_that_never_exits_is_released_after_the_grace_period() {
    let h = Harness::new(SessionConfig {
        max_sessions: 1,
        ..config()
    });
    let (id, remote) = h.open_ok();
    h.sessions.close(id).unwrap();
    settle().await;
    assert!(
        matches!(h.open(), Err(SessionError::LimitReached { .. })),
        "still within grace"
    );

    assert_eq!(remote.kills(), 0, "kill waits for the grace period");

    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(
        remote.kills(),
        1,
        "a stream that ignores termination is killed"
    );
    assert_eq!(h.sink.exits(), vec![None]);
    h.open_ok();
    drop(remote);
}

#[tokio::test(start_paused = true)]
async fn closing_while_behind_discards_output_instead_of_flooding_the_view() {
    let h = Harness::with_launcher(
        FakeLauncher::with_event_capacity(64),
        SessionConfig {
            flush_bytes: 4,
            watermarks: Watermarks::new(8, 4).unwrap(),
            ..config()
        },
    );
    let (id, remote) = h.open_ok();
    for _ in 0..3 {
        output(&remote, &[b'x'; 4]).await;
        settle().await;
    }
    let delivered_before_close = h.sink.output().len();
    h.sessions.close(id).unwrap();
    settle().await;

    // A process that ignores hang-up keeps writing; none of it reaches the view.
    for _ in 0..40 {
        output(&remote, &[b'y'; 4]).await;
    }
    remote
        .events
        .send(TerminalEvent::Exited { code: None })
        .await
        .unwrap();
    settle().await;
    assert_eq!(h.sink.output().len(), delivered_before_close);
    assert_eq!(h.sink.exits(), vec![None]);
}

#[tokio::test(start_paused = true)]
async fn in_flight_bytes_stay_within_high_watermark_plus_two_batches() {
    // H = 8, F = 4, chunks of up to C = 6: in flight must not exceed H + F + C = 18.
    let h = Harness::with_launcher(
        FakeLauncher::with_event_capacity(64),
        SessionConfig {
            flush_bytes: 4,
            watermarks: Watermarks::new(8, 4).unwrap(),
            ..config()
        },
    );
    let (_, remote) = h.open_ok();
    for len in [3, 6, 1, 6, 2, 5, 6, 6, 6, 6] {
        remote
            .events
            .try_send(TerminalEvent::Output(vec![b'z'; len]))
            .unwrap();
    }
    settle().await;
    let in_flight = h.sink.output().len();
    assert!(in_flight > 8, "paused only after crossing H");
    assert!(in_flight <= 18, "in flight {in_flight} > H + F + C");
}

#[tokio::test(start_paused = true)]
async fn a_probe_reaches_the_stream_and_a_lost_link_reaches_the_sink() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    // Every probe is let through, even with the input queue full.
    for _ in 0..64 {
        h.sessions.write(id, b"x".to_vec()).await.unwrap();
    }
    h.sessions.probe(id).unwrap();
    settle().await;
    assert_eq!(remote.probes(), 1);

    output(&remote, b"last words").await;
    remote.events.send(TerminalEvent::Lost).await.unwrap();
    settle().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // What came before the loss is shown first, then the loss, not an exit.
    assert_eq!(h.sink.output(), b"last words");
    assert_eq!(h.sink.lost.load(Ordering::Relaxed), 1);
    assert_eq!(h.sink.exits(), Vec::<Option<u32>>::new());
    assert!(matches!(
        h.sessions.probe(id),
        Err(SessionError::NotFound(_))
    ));
}

#[derive(Default)]
struct RecordingTap {
    output: Mutex<Vec<u8>>,
    sizes: Mutex<Vec<TerminalSize>>,
    ended: AtomicUsize,
}

impl OutputTap for RecordingTap {
    fn output(&self, bytes: &[u8]) {
        lock(&self.output).extend_from_slice(bytes);
    }

    fn resize(&self, size: TerminalSize) {
        lock(&self.sizes).push(size);
    }

    fn end(&self) {
        self.ended.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test(start_paused = true)]
async fn a_tap_hears_what_the_view_gets_and_the_end() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    output(&remote, b"before ").await;
    settle().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let tap = Arc::new(RecordingTap::default());
    let previous = h
        .sessions
        .set_tap(id, Some(Arc::clone(&tap) as Arc<dyn OutputTap>))
        .unwrap();
    assert!(previous.is_none());
    output(&remote, b"one ").await;
    output(&remote, b"two").await;
    let size = TerminalSize::new(100, 30).unwrap();
    h.sessions.resize(id, size).await.unwrap();
    remote
        .events
        .send(TerminalEvent::Exited { code: Some(0) })
        .await
        .unwrap();
    settle().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // What came before the tap is not its; the rest is, in order.
    assert_eq!(*lock(&tap.output), b"one two");
    assert_eq!(*lock(&tap.sizes), [size]);
    assert_eq!(tap.ended.load(Ordering::Relaxed), 1);
    assert_eq!(h.sink.output(), b"before one two");
}

#[tokio::test(start_paused = true)]
async fn a_tap_taken_off_hears_nothing_more_and_is_not_ended() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    let tap = Arc::new(RecordingTap::default());
    h.sessions
        .set_tap(id, Some(Arc::clone(&tap) as Arc<dyn OutputTap>))
        .unwrap();
    let off = h.sessions.set_tap(id, None).unwrap();
    assert!(off.is_some());
    output(&remote, b"after").await;
    settle().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(lock(&tap.output).is_empty());
    drop(remote);
    settle().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(tap.ended.load(Ordering::Relaxed), 0);
}

#[tokio::test(start_paused = true)]
async fn the_tail_keeps_the_last_output_across_chunks_and_counts_it_all() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    // More than the tail in one chunk, then a little: the tail rolls.
    let big = vec![b'x'; PROMPT_TAIL + 100];
    output(&remote, &big).await;
    output(&remote, b"[sudo] password for ops: ").await;
    settle().await;
    let (tail, received) = h.sessions.tail(id).unwrap();
    assert_eq!(tail.len(), PROMPT_TAIL);
    assert!(tail.ends_with(b"[sudo] password for ops: "));
    assert_eq!(received, (big.len() + 25) as u64);
    // One fill per output count.
    assert!(h.sessions.mark_filled(id, received).unwrap());
    assert!(!h.sessions.mark_filled(id, received).unwrap());
    output(&remote, b"\r\n").await;
    settle().await;
    let (_, after) = h.sessions.tail(id).unwrap();
    assert!(h.sessions.mark_filled(id, after).unwrap());
}

#[test]
fn recent_output_keeps_the_last_bytes_in_order_across_the_wrap() {
    let mut recent = Recent::default();
    let mut all = Vec::new();
    // Chunks of odd sizes, crossing the end of the ring many times.
    for (i, size) in [1, 700, RECENT_OUTPUT - 3, 5000, 1, RECENT_OUTPUT, 9000, 7]
        .into_iter()
        .enumerate()
    {
        let chunk: Vec<u8> = (0..size)
            .map(|j| u8::try_from((i * 31 + j) % 251).unwrap())
            .collect();
        recent.push(&chunk);
        all.extend_from_slice(&chunk);
        let keep = all.len().min(RECENT_OUTPUT);
        assert_eq!(
            recent.last(RECENT_OUTPUT),
            all[all.len() - keep..],
            "after {i}"
        );
        assert_eq!(
            recent.last(PROMPT_TAIL),
            all[all.len() - keep.min(PROMPT_TAIL)..],
            "tail after {i}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn recent_output_holds_what_the_session_showed() {
    let h = Harness::new(config());
    let (id, remote) = h.open_ok();
    output(&remote, b"make: *** [all] Error 2\r\n").await;
    settle().await;
    assert_eq!(
        h.sessions.recent(id).unwrap(),
        b"make: *** [all] Error 2\r\n"
    );
}

mod transfers {
    //! Files a host sends with `sz`, through a session's pump.

    use tauminus_domain::SessionKind;
    use zmodem2::{Action as Wire, Event as WireEvent, FileInfo, Position, Sender};

    use super::*;
    use tauminus_domain::Wants;

    use crate::ports::{
        PartFile, ReceiveFolder, TransferAnswer, TransferEvent, TransferHost, TransferKind,
    };

    #[derive(Debug, Default)]
    pub(super) struct Host {
        pub(super) offers: AtomicUsize,
        pub(super) wants: Mutex<Option<Wants>>,
        pub(super) kind: Mutex<Option<TransferKind>>,
        events: Mutex<Vec<TransferEvent>>,
    }

    impl TransferHost for Host {
        fn offered(&self, _id: SessionId, offer: u64, wants: Wants, kind: TransferKind) {
            *lock(&self.wants) = Some(wants);
            *lock(&self.kind) = Some(kind);
            let before = self.offers.fetch_add(1, Ordering::Relaxed);
            assert_eq!(offer, before as u64 + 1, "offers are numbered from 1");
        }

        fn event(&self, _id: SessionId, event: TransferEvent) {
            lock(&self.events).push(event);
        }
    }

    impl Host {
        pub(super) fn events(&self) -> Vec<TransferEvent> {
            lock(&self.events).clone()
        }
    }

    /// Files kept in memory, by name.
    #[derive(Debug, Default, Clone)]
    pub(super) struct Folder(
        pub(super) Arc<Mutex<HashMap<String, Vec<u8>>>>,
        /// Files discarded half written.
        pub(super) Arc<AtomicUsize>,
    );

    #[derive(Debug)]
    struct Part {
        name: String,
        bytes: Vec<u8>,
        into: Folder,
    }

    impl ReceiveFolder for Folder {
        fn create(&mut self, name: &str) -> std::io::Result<(String, Box<dyn PartFile>)> {
            Ok((
                name.to_owned(),
                Box::new(Part {
                    name: name.to_owned(),
                    bytes: Vec::new(),
                    into: self.clone(),
                }),
            ))
        }

        fn shown(&self) -> String {
            "Downloads".into()
        }
    }

    impl PartFile for Part {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }

        fn finish(self: Box<Self>) -> std::io::Result<()> {
            lock(&self.into.0).insert(self.name, self.bytes);
            Ok(())
        }

        fn discard(self: Box<Self>) {
            self.into.1.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct World {
        sessions: Arc<SessionManager>,
        id: SessionId,
        events: mpsc::Sender<TerminalEvent>,
        input: mpsc::Receiver<TerminalInput>,
        host: Arc<Host>,
        sink: RecordingSink,
    }

    /// A session of a stream the caller runs.
    pub(super) struct Attached {
        pub(super) sessions: Arc<SessionManager>,
        pub(super) id: SessionId,
        pub(super) host: Arc<Host>,
        pub(super) sink: RecordingSink,
    }

    impl Attached {
        pub(super) fn with_stream(
            input_tx: mpsc::Sender<TerminalInput>,
            events: mpsc::Receiver<TerminalEvent>,
        ) -> Self {
            let sessions = Arc::new(SessionManager::new(config()));
            let host = Arc::new(Host::default());
            sessions.set_transfer_host(Arc::clone(&host) as Arc<dyn TransferHost>);
            let sink = RecordingSink::default();
            let id = attach(&sessions, input_tx, events, &sink);
            Self {
                sessions,
                id,
                host,
                sink,
            }
        }
    }

    fn attach(
        sessions: &SessionManager,
        input_tx: mpsc::Sender<TerminalInput>,
        events: mpsc::Receiver<TerminalEvent>,
        sink: &RecordingSink,
    ) -> SessionId {
        sessions.attach(
            sessions.reserve().unwrap(),
            TerminalStream {
                input: input_tx,
                events,
                terminator: Box::new(CountingTerminator {
                    terminated: Arc::default(),
                    killed: Arc::default(),
                    probed: Arc::default(),
                }),
            },
            sink.clone(),
            SessionMeta {
                transfers: true,
                ..SessionMeta::of(SessionKind::Ssh)
            },
        )
    }

    fn world() -> World {
        let sessions = Arc::new(SessionManager::new(config()));
        let host = Arc::new(Host::default());
        sessions.set_transfer_host(Arc::clone(&host) as Arc<dyn TransferHost>);
        let (events_tx, events) = mpsc::channel(64);
        let (input_tx, input) = mpsc::channel(64);
        let sink = RecordingSink::default();
        let id = attach(&sessions, input_tx, events, &sink);
        World {
            sessions,
            id,
            events: events_tx,
            input,
            host,
            sink,
        }
    }

    pub(super) async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..400 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("never: {what}");
    }

    const OFFER: &[u8] = b"rz\r**\x18B00000000000000\r\x8a\x11";

    #[tokio::test]
    async fn an_offer_waits_for_the_user_and_a_decline_shows_it_as_it_came() {
        let mut w = world();
        let mut printed = b"$ cat notes\r\n".to_vec();
        printed.extend_from_slice(OFFER);
        w.events.send(TerminalEvent::Output(printed)).await.unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        // Shown up to the offer; nothing sent back; no keys in.
        until("the text before", || {
            w.sink.output() == b"$ cat notes\r\nrz\r"
        })
        .await;
        assert!(w.input.try_recv().is_err());
        assert!(matches!(
            w.sessions.write(w.id, b"ls\r".to_vec()).await,
            Err(SessionError::Transferring)
        ));
        w.events
            .send(TerminalEvent::Output(b" more".to_vec()))
            .await
            .unwrap();
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Decline)
            .unwrap();
        let mut whole = b"$ cat notes\r\n".to_vec();
        whole.extend_from_slice(OFFER);
        whole.extend_from_slice(b" more");
        until("the held output", || w.sink.output() == whole).await;
        assert!(w.input.try_recv().is_err(), "nothing went to the host");
        w.sessions.write(w.id, b"ls\r".to_vec()).await.unwrap();
        assert_eq!(w.host.events(), vec![TransferEvent::Declined]);
        // `sz` asking again at once is not offered again.
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(w.host.offers.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn after_a_stop_the_rest_of_the_files_is_dropped_and_text_shows_again() {
        let mut w = world();
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Receive(Box::new(Folder::default())),
            )
            .unwrap();
        until("the start", || !w.host.events().is_empty()).await;
        let shown = w.sink.output().len();
        w.sessions.cancel_transfer(w.id).unwrap();
        until("the stop", || {
            matches!(w.host.events().last(), Some(TransferEvent::Failed { .. }))
        })
        .await;
        // The host is told to stop: CAN, five or more.
        let mut sent = Vec::new();
        while let Ok(TerminalInput::Data(bytes)) = w.input.try_recv() {
            sent.extend(bytes);
        }
        assert!(sent.windows(5).any(|x| x == [0x18; 5]), "{sent:?}");
        // What it had sent already: data with ZDLE, and bytes that are not
        // UTF-8.
        for chunk in [&b"\x93\x18h\x01\xfe"[..], &[0xc3, 0x28, 0x00, 0xff]] {
            w.events
                .send(TerminalEvent::Output(chunk.to_vec()))
                .await
                .unwrap();
        }
        // Text of a file, cut where it holds no ZDLE: nobody typed, so it
        // is still the file's, before and after the host goes quiet.
        w.events
            .send(TerminalEvent::Output(b"db_password=hunter2\n".to_vec()))
            .await
            .unwrap();
        // Ctrl+C again, as users press it: not typing.
        w.sessions.write(w.id, b"\x03".to_vec()).await.unwrap();
        w.events
            .send(TerminalEvent::Output(b"AWS_SECRET=abc123\n".to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(w.sink.output().len(), shown, "nothing of the file shows");
        // Typed: the shell answers, and shows.
        w.sessions.write(w.id, b"\r".to_vec()).await.unwrap();
        w.events
            .send(TerminalEvent::Output(b"\r\n$ ".to_vec()))
            .await
            .unwrap();
        until("the prompt", || w.sink.output().ends_with(b"\r\n$ ")).await;
        assert_eq!(&w.sink.output()[shown..], b"\r\n$ ");
    }

    #[tokio::test]
    async fn typing_during_the_stop_still_drops_file_data() {
        let mut w = world();
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Receive(Box::new(Folder::default())),
            )
            .unwrap();
        until("the start", || !w.host.events().is_empty()).await;
        let shown = w.sink.output().len();
        w.sessions.cancel_transfer(w.id).unwrap();
        until("the stop", || {
            matches!(w.host.events().last(), Some(TransferEvent::Failed { .. }))
        })
        .await;
        while w.input.try_recv().is_ok() {}
        w.sessions.write(w.id, b"x".to_vec()).await.unwrap();
        // Typed, but this is the file's: it carries ZDLE.
        w.events
            .send(TerminalEvent::Output(b"data\x18hmore".to_vec()))
            .await
            .unwrap();
        w.events
            .send(TerminalEvent::Output(b"x".to_vec()))
            .await
            .unwrap();
        until("the echo", || w.sink.output().len() > shown).await;
        assert_eq!(&w.sink.output()[shown..], b"x");
    }

    #[tokio::test]
    async fn an_offer_cut_across_reads_is_found_and_shown_once_when_declined() {
        for cut in [4, 8, 12, 20] {
            let w = world();
            let mut printed = b"motd ".to_vec();
            printed.extend_from_slice(OFFER);
            let (first, second) = printed.split_at(cut);
            for part in [first, second] {
                w.events
                    .send(TerminalEvent::Output(part.to_vec()))
                    .await
                    .unwrap();
            }
            until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
            w.sessions
                .answer_transfer(w.id, 1, TransferAnswer::Decline)
                .unwrap();
            until("the held output", || w.sink.output() == printed).await;
        }
    }

    #[tokio::test]
    async fn an_offer_left_waiting_past_256_kib_is_declined_and_shown_whole() {
        let w = world();
        let mut whole = OFFER.to_vec();
        let mut chunks = vec![OFFER.to_vec()];
        for i in 0..40u8 {
            let chunk = vec![b'a' + i % 26; 8 << 10];
            whole.extend_from_slice(&chunk);
            chunks.push(chunk);
        }
        // Sent from a task: the session holds back while the view is behind.
        let events = w.events.clone();
        tokio::spawn(async move {
            for chunk in chunks {
                events.send(TerminalEvent::Output(chunk)).await.unwrap();
            }
        });
        // The view keeps up, acknowledging what it parsed.
        let mut acked = 0;
        until("all of it", || {
            let shown = w.sink.output().len();
            w.sessions.acknowledge(w.id, shown - acked).unwrap();
            acked = shown;
            w.sink.output() == whole
        })
        .await;
        assert_eq!(w.host.events(), vec![TransferEvent::Declined]);
    }

    #[tokio::test]
    async fn a_host_that_reads_no_replies_is_stopped() {
        let w = world();
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Receive(Box::new(Folder::default())),
            )
            .unwrap();
        // Headers that each get a reply, and the test reads none of them.
        let header = &OFFER[3..];
        for _ in 0..40 {
            w.events
                .send(TerminalEvent::Output(header.repeat(1000)))
                .await
                .unwrap();
            if matches!(w.host.events().last(), Some(TransferEvent::Failed { .. })) {
                break;
            }
        }
        until("the stop", || {
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Failed { reason }) if reason.contains("stopped reading")
            )
        })
        .await;
    }

    #[tokio::test]
    async fn ctrl_c_declines_an_offer() {
        let w = world();
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        // Typed with other keys, in one write.
        w.sessions.write_text(w.id, "q\u{3}".into()).await.unwrap();
        until("the decline", || {
            w.host.events() == vec![TransferEvent::Declined]
        })
        .await;
    }

    type Files = [(&'static [u8], Vec<u8>)];

    /// A host running `sz` for `files` against the session, until it is
    /// done (true) or `stop` holds (false). `waiting` runs whenever the
    /// host waits for the session.
    async fn host_sends(
        w: &mut World,
        files: &Files,
        mut waiting: impl FnMut(&World),
        stop: impl Fn(&World) -> bool,
    ) -> bool {
        let mut sender = Sender::new().unwrap();
        let size = |data: &Vec<u8>| Some(Position::new(u32::try_from(data.len()).unwrap()));
        sender
            .start_file(FileInfo::new(files[0].0, size(&files[0].1)))
            .unwrap();
        let mut index = 0;
        let mut to_sender: Vec<u8> = Vec::new();
        for _ in 0..200_000 {
            if stop(w) {
                return false;
            }
            match sender.poll() {
                Wire::WriteWire(bytes) => {
                    let n = bytes.len();
                    w.events
                        .send(TerminalEvent::Output(bytes.to_vec()))
                        .await
                        .unwrap();
                    sender.wire_written(n);
                }
                Wire::ReadFile { offset, max_len } => {
                    let data = &files[index].1;
                    let from = offset.get() as usize;
                    let to = (from + max_len).min(data.len());
                    sender.submit_file(&data[from..to]).unwrap();
                }
                Wire::Event(WireEvent::FileCompleted) => {
                    index += 1;
                    if let Some((name, data)) = files.get(index) {
                        sender.start_file(FileInfo::new(name, size(data))).unwrap();
                    } else {
                        sender.finish().unwrap();
                    }
                }
                Wire::Event(WireEvent::SessionCompleted) => return true,
                _ => {
                    waiting(w);
                    if let Ok(Some(TerminalInput::Data(reply))) =
                        tokio::time::timeout(Duration::from_millis(20), w.input.recv()).await
                    {
                        to_sender.extend(reply);
                    }
                    let taken = sender.submit_wire(&to_sender).unwrap();
                    to_sender.drain(..taken);
                }
            }
        }
        panic!("the host never finished");
    }

    fn big() -> Vec<u8> {
        (0..300_000u32).map(|i| (i * 7).to_le_bytes()[0]).collect()
    }

    /// Accepts offer 1 into `folder` once it is made.
    fn accept(folder: &Folder) -> impl FnMut(&World) + '_ {
        move |w: &World| {
            if w.host.offers.load(Ordering::Relaxed) == 1 && w.host.events().is_empty() {
                w.sessions
                    .answer_transfer(w.id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
                    .unwrap();
            }
        }
    }

    fn started(w: &World) -> usize {
        w.host
            .events()
            .iter()
            .filter(|e| matches!(e, TransferEvent::Started { .. }))
            .count()
    }

    #[tokio::test]
    async fn accepted_files_are_saved_and_never_shown_or_recorded() {
        let mut w = world();
        let folder = Folder::default();
        let files = [
            (&b"a.txt"[..], b"hello\n".to_vec()),
            (&b"big.bin"[..], big()),
        ];
        // Clicked twice: every wait answers, as a double click would.
        let done = host_sends(
            &mut w,
            &files,
            |w| {
                if w.host.offers.load(Ordering::Relaxed) == 1 {
                    w.sessions
                        .answer_transfer(w.id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
                        .unwrap();
                }
            },
            |_| false,
        )
        .await;
        assert!(done, "the host finished");
        w.events
            .send(TerminalEvent::Output(b"OO\r\n$ ".to_vec()))
            .await
            .unwrap();
        until("the prompt after", || w.sink.output().ends_with(b"$ ")).await;
        assert_eq!(lock(&folder.0).get("a.txt"), Some(&b"hello\n".to_vec()));
        assert!(
            lock(&folder.0)
                .get("big.bin")
                .is_some_and(|got| *got == big())
        );
        // The view saw the prompt alone: no file byte, no header, no `OO`.
        assert_eq!(w.sink.output(), b"$ ");
        assert!(
            !w.sessions
                .recent(w.id)
                .unwrap()
                .windows(6)
                .any(|x| x == b"hello\n")
        );
        assert!(matches!(
            w.host.events().last(),
            Some(TransferEvent::Finished { files, more_files: 0, .. })
                if files == &vec!["a.txt".to_owned(), "big.bin".to_owned()]
        ));
        w.sessions.write(w.id, b"ls\r".to_vec()).await.unwrap();
        while w.input.try_recv().is_ok() {}

        // A cancel with nothing to cancel, the answers left from the double
        // click, and one given ahead for the next offer, do nothing to it:
        // it waits for the user.
        w.sessions.cancel_transfer(w.id).unwrap();
        w.sessions
            .answer_transfer(w.id, 2, TransferAnswer::Receive(Box::new(folder.clone())))
            .unwrap();
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        until("the second offer", || {
            w.host.offers.load(Ordering::Relaxed) == 2
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(started(&w), 1);
        assert!(matches!(
            w.host.events().last(),
            Some(TransferEvent::Finished { .. })
        ));
        assert!(w.input.try_recv().is_err(), "nothing went to the host");
        // A cancel asked for offer 1 that reaches the pump only now: it
        // stops nothing of offer 2.
        {
            let control = w.sessions.control(w.id).unwrap();
            control.transfer_cancel_offer.store(1, Ordering::Relaxed);
            control.transfer_cancel.notify_one();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(matches!(
            w.host.events().last(),
            Some(TransferEvent::Finished { .. })
        ));
        // Its own number answers it.
        w.sessions
            .answer_transfer(w.id, 2, TransferAnswer::Decline)
            .unwrap();
        until("the decline", || {
            matches!(w.host.events().last(), Some(TransferEvent::Declined))
        })
        .await;
    }

    #[tokio::test]
    async fn a_stop_mid_file_removes_it_and_tells_the_host() {
        let mut w = world();
        let folder = Folder::default();
        let files = [(&b"big.bin"[..], big())];
        let receiving = |w: &World| {
            w.host
                .events()
                .iter()
                .any(|e| matches!(e, TransferEvent::File { .. }))
        };
        assert!(!host_sends(&mut w, &files, accept(&folder), receiving).await);
        w.sessions.cancel_transfer(w.id).unwrap();
        until("the stop", || {
            matches!(w.host.events().last(), Some(TransferEvent::Failed { .. }))
        })
        .await;
        assert_eq!(folder.1.load(Ordering::Relaxed), 1, "the part file went");
        assert!(lock(&folder.0).is_empty());
        let mut sent = Vec::new();
        while let Ok(TerminalInput::Data(bytes)) = w.input.try_recv() {
            sent.extend(bytes);
        }
        assert!(sent.windows(5).any(|x| x == [0x18; 5]));
    }

    #[tokio::test]
    async fn a_session_that_ends_mid_file_removes_it_and_says_so() {
        let mut w = world();
        let folder = Folder::default();
        let files = [(&b"big.bin"[..], big())];
        let receiving = |w: &World| {
            w.host
                .events()
                .iter()
                .any(|e| matches!(e, TransferEvent::File { .. }))
        };
        assert!(!host_sends(&mut w, &files, accept(&folder), receiving).await);
        w.events.send(TerminalEvent::Lost).await.unwrap();
        until("the end", || {
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Failed { reason }) if reason == "the session ended"
            )
        })
        .await;
        assert_eq!(folder.1.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn closing_mid_file_shows_none_of_it() {
        let mut w = world();
        let folder = Folder::default();
        let files = [(&b"big.bin"[..], big())];
        let receiving = |w: &World| {
            w.host
                .events()
                .iter()
                .any(|e| matches!(e, TransferEvent::File { .. }))
        };
        assert!(!host_sends(&mut w, &files, accept(&folder), receiving).await);
        let shown = w.sink.output().len();
        w.sessions.close(w.id).unwrap();
        w.events
            .send(TerminalEvent::Output(b"secret file text".to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(w.sink.output().len(), shown);
    }

    /// A local file in memory.
    #[derive(Debug)]
    struct MemFile(&'static str, Vec<u8>);

    impl crate::ports::OutgoingFile for MemFile {
        fn name(&self) -> &str {
            self.0
        }

        fn size(&self) -> u64 {
            self.1.len() as u64
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let from = usize::try_from(offset).unwrap().min(self.1.len());
            let n = buf.len().min(self.1.len() - from);
            buf[..n].copy_from_slice(&self.1[from..from + n]);
            Ok(n)
        }
    }

    /// Files a host got, by name, in order.
    type Got = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    /// The ZRINIT `rz` prints as it starts.
    const RZ: &[u8] = b"rz waiting to receive.**\x18B0100000023be50\r\x8a\x11";

    /// `rz` on the host: reads the session's input, answers through its
    /// output, and keeps the files, by name, in `got`. It skips `skip`.
    fn rz(
        mut input: mpsc::Receiver<TerminalInput>,
        events: mpsc::Sender<TerminalEvent>,
        got: Got,
        skip: &'static str,
        last: &'static [u8],
    ) {
        tokio::spawn(async move {
            let mut host = zmodem2::Receiver::new().unwrap();
            host.set_manual_file_accept(true);
            while let Some(message) = input.recv().await {
                let TerminalInput::Data(bytes) = message else {
                    continue;
                };
                let mut at = 0;
                loop {
                    let mut reply = Vec::new();
                    let mut ended = false;
                    loop {
                        match host.poll() {
                            Wire::WriteWire(out) => {
                                let n = out.len();
                                reply.extend_from_slice(out);
                                host.wire_written(n);
                            }
                            Wire::Event(WireEvent::FileStarted(info)) => {
                                let name = String::from_utf8_lossy(info.name).into_owned();
                                if name == skip {
                                    host.skip_file().unwrap();
                                } else {
                                    lock(&got).push((name, Vec::new()));
                                    host.accept_file_at(0).unwrap();
                                }
                            }
                            Wire::WriteFile(data) => {
                                let n = data.len();
                                lock(&got).last_mut().unwrap().1.extend_from_slice(data);
                                host.file_written(n).unwrap();
                            }
                            // `rz` exits; the shell's prompt comes in the
                            // same read, after its last reply.
                            Wire::Event(WireEvent::SessionCompleted) => ended = true,
                            Wire::Idle => break,
                            _ => {}
                        }
                    }
                    if ended {
                        reply.extend_from_slice(last);
                    }
                    if !reply.is_empty() && events.send(TerminalEvent::Output(reply)).await.is_err()
                    {
                        return;
                    }
                    if at >= bytes.len() {
                        break;
                    }
                    match host.submit_wire(&bytes[at..]) {
                        Ok(n) => at += n,
                        Err(_) => break,
                    }
                }
            }
        });
    }

    /// A session whose host runs `rz`: the files it got.
    fn with_rz(skip: &'static str) -> (World, Got) {
        with_rz_then(skip, b"")
    }

    /// `rz` that prints `last` with its last reply.
    fn with_rz_then(skip: &'static str, last: &'static [u8]) -> (World, Got) {
        let w = world();
        let (input_tx, input) = mpsc::channel(64);
        // The test's world reads input itself; this one gives it to `rz`.
        let sessions = Arc::clone(&w.sessions);
        let host = Arc::clone(&w.host);
        let sink = w.sink.clone();
        let (events_tx, events) = mpsc::channel(64);
        let id = attach(&sessions, input_tx, events, &sink);
        let got = Arc::default();
        rz(input, events_tx.clone(), Arc::clone(&got), skip, last);
        let (_, dummy) = mpsc::channel(1);
        (
            World {
                sessions,
                id,
                events: events_tx,
                input: dummy,
                host,
                sink,
            },
            got,
        )
    }

    fn files(w: &World) -> Vec<Box<dyn crate::ports::OutgoingFile>> {
        let _ = w;
        vec![
            Box::new(MemFile("a.txt", b"hello\n".to_vec())),
            Box::new(MemFile("skip.me", vec![7; 3000])),
            Box::new(MemFile("big.bin", big())),
        ]
    }

    #[tokio::test]
    async fn files_go_to_rz_once_the_user_chose_them() {
        let (w, got) = with_rz("skip.me");
        w.events
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        assert_eq!(*lock(&w.host.wants), Some(Wants::Send));
        // Shown up to the header; keys refused; the wrong kind of answer
        // does nothing.
        until("the text", || w.sink.output() == b"rz waiting to receive.").await;
        assert!(matches!(
            w.sessions.write(w.id, b"x".to_vec()).await,
            Err(SessionError::Transferring)
        ));
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Receive(Box::new(Folder::default())),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(w.host.events(), vec![]);
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Send(files(&w)))
            .unwrap();
        until("the end", || {
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Finished { .. } | TransferEvent::Failed { .. })
            )
        })
        .await;
        assert!(
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Finished { folder: None, files, skipped, .. })
                    if files == &["a.txt", "big.bin"] && skipped == &["skip.me"]
            ),
            "{:?}",
            w.host.events()
        );
        let got = lock(&got).clone();
        assert_eq!(got[0], ("a.txt".to_owned(), b"hello\n".to_vec()));
        assert_eq!(got[1].0, "big.bin");
        assert!(got[1].1.eq(&big()));
        // Nothing of the protocol reached the view.
        assert_eq!(w.sink.output(), b"rz waiting to receive.");
        w.sessions.write(w.id, b"ls\r".to_vec()).await.unwrap();
    }

    #[tokio::test]
    async fn a_stop_mid_send_tells_rz_and_frees_the_keys() {
        let (w, got) = with_rz("");
        w.events
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        let huge: Vec<Box<dyn crate::ports::OutgoingFile>> =
            vec![Box::new(MemFile("huge.bin", vec![1; 64 << 20]))];
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Send(huge))
            .unwrap();
        until("data", || {
            lock(&got).first().is_some_and(|f| !f.1.is_empty())
        })
        .await;
        w.sessions.write(w.id, b"\x03".to_vec()).await.unwrap();
        until("the stop", || {
            matches!(w.host.events().last(), Some(TransferEvent::Failed { .. }))
        })
        .await;
        // Not all 64 MiB went: the stop came between.
        assert!(lock(&got)[0].1.len() < 64 << 20);
        w.sessions.write(w.id, b"ls\r".to_vec()).await.unwrap();
    }

    #[tokio::test]
    async fn when_rz_stops_by_itself_nothing_queued_reaches_the_host_after() {
        // `rz` as lrzsz runs it: it takes a stream (ZRINIT with no buffer
        // and CANOVIO), reads slowly, and stops after 10 KB, as on a full
        // disk. What reaches it after that is counted.
        let w = world();
        let (input_tx, mut input) = mpsc::channel(64);
        let (events_tx, events) = mpsc::channel(64);
        let id = attach(&w.sessions, input_tx, events, &w.sink);
        let after = Arc::new(AtomicUsize::new(0));
        let late = Arc::clone(&after);
        let replies = events_tx.clone();
        tokio::spawn(async move {
            let zrpos0 = b"**\x18B0900000000a87c\r\x8a\x11".to_vec();
            let (mut messages, mut read, mut stopped) = (0, 0, false);
            while let Some(TerminalInput::Data(bytes)) = input.recv().await {
                messages += 1;
                if stopped {
                    late.fetch_add(bytes.len(), Ordering::Relaxed);
                    continue;
                }
                match messages {
                    // Our ZRQINIT, then ZFILE.
                    1 => replies
                        .send(TerminalEvent::Output(RZ[22..].to_vec()))
                        .await
                        .unwrap(),
                    2 => replies
                        .send(TerminalEvent::Output(zrpos0.clone()))
                        .await
                        .unwrap(),
                    _ => {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        read += bytes.len();
                        if read > 10_000 {
                            stopped = true;
                            replies
                                .send(TerminalEvent::Output(vec![0x18; 10]))
                                .await
                                .unwrap();
                        }
                    }
                }
            }
        });
        events_tx
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        let huge: Vec<Box<dyn crate::ports::OutgoingFile>> =
            vec![Box::new(MemFile("huge.bin", vec![b'x'; 64 << 20]))];
        w.sessions
            .answer_transfer(id, 1, TransferAnswer::Send(huge))
            .unwrap();
        until("the stop", || {
            matches!(w.host.events().last(), Some(TransferEvent::Failed { .. }))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // What the session's input queue held (64 writes of about 1 KiB)
        // still goes; what was queued past it does not.
        let late = after.load(Ordering::Relaxed);
        assert!(late < 80 << 10, "{late} bytes after rz stopped");
    }

    /// A file that is shorter when read than when chosen.
    #[derive(Debug)]
    struct Shrunk;

    impl crate::ports::OutgoingFile for Shrunk {
        fn name(&self) -> &'static str {
            "log.txt"
        }

        fn size(&self) -> u64 {
            100_000
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let left = 5000u64.saturating_sub(offset);
            let n = buf.len().min(usize::try_from(left).unwrap());
            buf[..n].fill(b'a');
            Ok(n)
        }
    }

    #[tokio::test]
    async fn a_file_that_shrank_stops_the_send() {
        let (w, _) = with_rz("");
        w.events
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Send(vec![Box::new(Shrunk)]))
            .unwrap();
        until("the stop", || {
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Failed { reason }) if reason.contains("changed")
            )
        })
        .await;
    }

    #[tokio::test]
    async fn files_are_not_sent_to_sz_nor_received_from_rz() {
        let w = world();
        w.events
            .send(TerminalEvent::Output(OFFER.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        assert_eq!(*lock(&w.host.wants), Some(Wants::Receive));
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Send(vec![Box::new(MemFile("a", vec![1]))]),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(w.host.events(), vec![]);
        assert_eq!(
            w.sessions.waiting_offer(w.id).unwrap(),
            Some((1, Wants::Receive))
        );
    }

    #[tokio::test]
    async fn the_prompt_in_rz_s_last_reply_is_shown() {
        let (w, _) = with_rz_then("", b"\r\n$ ");
        w.events
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Send(vec![Box::new(MemFile("a", b"x".to_vec()))]),
            )
            .unwrap();
        until("the prompt", || w.sink.output().ends_with(b"$ ")).await;
        assert!(matches!(
            w.host.events().last(),
            Some(TransferEvent::Finished { .. })
        ));
    }

    #[tokio::test]
    async fn the_prompt_after_a_finished_send_is_shown() {
        let (w, _) = with_rz("");
        w.events
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Send(vec![Box::new(MemFile("a", b"x".to_vec()))]),
            )
            .unwrap();
        until("the end", || {
            matches!(w.host.events().last(), Some(TransferEvent::Finished { .. }))
        })
        .await;
        w.events
            .send(TerminalEvent::Output(b"\r\n$ ".to_vec()))
            .await
            .unwrap();
        until("the prompt", || w.sink.output().ends_with(b"$ ")).await;
    }

    #[tokio::test]
    async fn a_send_to_an_rz_that_is_gone_ends_though_the_shell_prints() {
        // `rz` gave up while the user chose; the shell prints a clock line
        // every half second, and never answers as `rz`.
        let w = world();
        w.events
            .send(TerminalEvent::Output(RZ.to_vec()))
            .await
            .unwrap();
        w.events
            .send(TerminalEvent::Output(b"rz: timed out\r\n$ ".to_vec()))
            .await
            .unwrap();
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Send(vec![Box::new(MemFile("a", vec![1; 10]))]),
            )
            .unwrap();
        let clock = w.events.clone();
        let ticking = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if clock
                    .send(TerminalEvent::Output(b"12:00:01 tick\r\n".to_vec()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        for _ in 0..80 {
            if matches!(w.host.events().last(), Some(TransferEvent::Failed { .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ticking.abort();
        assert!(
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Failed { reason }) if reason.contains("not waiting")
            ),
            "{:?}",
            w.host.events()
        );
        // What the shell printed while the user chose is shown.
        until("the held output", || {
            String::from_utf8_lossy(&w.sink.output()).contains("rz: timed out\r\n$ ")
        })
        .await;
    }

    // Files a host sends with `tsz` (ADR 0031).

    use tauminus_domain::trzsz::{decode, encode, frame};

    const TRIGGER: &[u8] = b"\x1b[s::TRZSZ:TRANSFER:S:1.2.0:1729812345600:0\r\n";

    /// The next `n` lines the terminal wrote to the host; Ctrl+C alone
    /// counts as one.
    async fn said(w: &mut World, n: usize) -> Vec<String> {
        let mut wire = Vec::new();
        loop {
            let text = String::from_utf8(wire.clone()).unwrap();
            let lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
            let whole = lines
                .iter()
                .filter(|l| l.ends_with('\n') || l.as_str() == "\x03")
                .count();
            if whole >= n {
                return lines;
            }
            let Some(TerminalInput::Data(bytes)) =
                tokio::time::timeout(Duration::from_secs(2), w.input.recv())
                    .await
                    .expect("a line from the terminal")
            else {
                continue;
            };
            wire.extend(bytes);
        }
    }

    async fn host(w: &World, bytes: Vec<u8>) {
        w.events.send(TerminalEvent::Output(bytes)).await.unwrap();
    }

    fn text(kind: &str, value: &[u8]) -> Vec<u8> {
        frame(kind, &encode(value))
    }

    async fn offered(w: &mut World) {
        let mut printed = b"$ tsz notes.txt\r\n".to_vec();
        printed.extend_from_slice(TRIGGER);
        host(w, printed).await;
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        assert_eq!(*lock(&w.host.kind), Some(TransferKind::Trzsz));
        assert_eq!(*lock(&w.host.wants), Some(Wants::Receive));
        // Shown up to the mark: the cursor it saves is the host's.
        until("the text before", || {
            w.sink.output() == b"$ tsz notes.txt\r\n\x1b[s"
        })
        .await;
    }

    #[tokio::test]
    async fn a_file_from_tsz_is_saved_and_the_host_s_message_shown() {
        let mut w = world();
        offered(&mut w).await;
        let folder = Folder::default();
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
            .unwrap();
        assert!(said(&mut w, 1).await[0].starts_with("#ACT:"));
        let body = b"line one\nline two\n".repeat(200);
        let mut lines = text("CFG", br#"{"timeout":20}"#);
        lines.extend(frame("NUM", "1"));
        lines.extend(text("NAME", b"notes.txt"));
        host(&w, lines).await;
        let lines = said(&mut w, 2).await;
        assert_eq!(lines[0], "#SUCC:1\n");
        let name = decode(&lines[1].trim_end().as_bytes()["#SUCC:".len()..], 64).unwrap();
        assert_eq!(name, b"notes.txt");
        let mut lines = frame("SIZE", &body.len().to_string());
        lines.extend(text("DATA", &body));
        host(&w, lines).await;
        let size = format!("#SUCC:{}\n", body.len());
        assert_eq!(said(&mut w, 2).await, [size.clone(), size]);
        host(&w, text("MD5", &tauminus_domain::trzsz::md5(&body))).await;
        let end = said(&mut w, 2).await;
        assert!(end[0].starts_with("#SUCC:") && end[1].starts_with("#EXIT:"));
        host(&w, b"\x1b[u\x1b[0JSaved 1 file\r\n$ ".to_vec()).await;
        until("the prompt", || w.sink.output().ends_with(b"$ ")).await;
        assert_eq!(lock(&folder.0).get("notes.txt"), Some(&body));
        assert!(matches!(
            w.host.events().last(),
            Some(TransferEvent::Finished { files, .. }) if files == &["notes.txt"]
        ));
        // The file's text never reached the view.
        let shown = w.sink.output();
        assert!(!shown.windows(8).any(|x| x == b"line one"));
        // The same trigger again is old output: shown, not offered.
        host(&w, TRIGGER.to_vec()).await;
        until("the old trigger", || w.sink.output().ends_with(b"\r\n")).await;
        assert_eq!(w.host.offers.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_decline_sends_ctrl_c_and_folders_and_windows_are_told_not_offered() {
        let mut w = world();
        offered(&mut w).await;
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Decline)
            .unwrap();
        assert_eq!(said(&mut w, 1).await, ["\x03"]);
        for (trigger, reason) in [
            (
                &b"::TRZSZ:TRANSFER:D:1.2.0:1729812345800:0\r\n"[..],
                "folders",
            ),
            (b"::TRZSZ:TRANSFER:S:1.2.0:1729812345810:0\r\n", "Windows"),
        ] {
            let mut w = world();
            host(&w, trigger.to_vec()).await;
            until("the reason", || !w.host.events().is_empty()).await;
            assert!(matches!(
                w.host.events().last(),
                Some(TransferEvent::Failed { reason: why }) if why.contains(reason)
            ));
            until("shown", || w.sink.output() == trigger).await;
            assert_eq!(w.host.offers.load(Ordering::Relaxed), 0);
            assert!(
                w.sessions.write(w.id, b"\x03".to_vec()).await.is_ok(),
                "keys go"
            );
            assert!(w.input.try_recv().is_ok());
        }
    }

    #[tokio::test]
    async fn files_go_to_trz_whole_as_base64_lines() {
        let mut w = world();
        host(
            &w,
            b"\x1b[s::TRZSZ:TRANSFER:R:1.2.0:1729812345900:0\r\n".to_vec(),
        )
        .await;
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        assert_eq!(*lock(&w.host.wants), Some(Wants::Send));
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        w.sessions
            .answer_transfer(
                w.id,
                1,
                TransferAnswer::Send(vec![
                    Box::new(MemFile("a.txt", b"hello\n".to_vec())),
                    Box::new(MemFile("big.bin", big.clone())),
                ]),
            )
            .unwrap();
        // trz: answers each line as it comes, and keeps the files.
        let mut got: Vec<(String, Vec<u8>)> = Vec::new();
        let mut exit = None;
        while exit.is_none() {
            for line in said(&mut w, 1).await {
                let line = line.trim_end_matches('\n');
                let (kind, payload) = line.split_once(':').unwrap();
                let value = || decode(payload.as_bytes(), 4 << 20).unwrap();
                let answer = match kind {
                    "#ACT" => frame("CFG", &encode(b"{}")),
                    "#NUM" | "#SIZE" | "#MD5" => frame("SUCC", payload),
                    "#NAME" => {
                        got.push((String::from_utf8(value()).unwrap(), Vec::new()));
                        frame("SUCC", payload)
                    }
                    "#DATA" => {
                        assert!(
                            payload
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
                        );
                        let bytes = value();
                        got.last_mut().unwrap().1.extend_from_slice(&bytes);
                        frame("SUCC", &bytes.len().to_string())
                    }
                    "#EXIT" => {
                        exit = Some(String::from_utf8(value()).unwrap());
                        b"\x1b[u\x1b[0JReceived 2 files\r\n$ ".to_vec()
                    }
                    other => panic!("{other}"),
                };
                host(&w, answer).await;
            }
        }
        assert_eq!(
            got,
            [
                ("a.txt".to_owned(), b"hello\n".to_vec()),
                ("big.bin".to_owned(), big)
            ]
        );
        until("the end", || {
            matches!(w.host.events().last(), Some(TransferEvent::Finished { files, .. }) if files.len() == 2)
        })
        .await;
        until("the prompt", || w.sink.output().ends_with(b"$ ")).await;
        assert!(w.sessions.write(w.id, b"ls\r".to_vec()).await.is_ok());
    }

    #[tokio::test]
    async fn a_stop_mid_file_sends_ctrl_c_drops_the_rest_and_shows_the_host_s_end() {
        let mut w = world();
        offered(&mut w).await;
        let folder = Folder::default();
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
            .unwrap();
        said(&mut w, 1).await;
        let mut lines = text("CFG", b"{}");
        lines.extend(frame("NUM", "1"));
        lines.extend(text("NAME", b"secret.env"));
        host(&w, lines).await;
        said(&mut w, 2).await;
        host(&w, frame("SIZE", "100")).await;
        said(&mut w, 1).await;
        let shown = w.sink.output().len();
        w.sessions.cancel_transfer(w.id).unwrap();
        assert_eq!(said(&mut w, 1).await, ["\x03"]);
        until("the stop", || {
            matches!(w.host.events().last(), Some(TransferEvent::Failed { .. }))
        })
        .await;
        assert_eq!(folder.1.load(Ordering::Relaxed), 1, "the part file goes");
        // A data line on its way, as text: dropped.
        host(&w, text("DATA", b"DB_PASSWORD=hunter2")).await;
        w.sessions.write(w.id, b"\r".to_vec()).await.unwrap();
        host(&w, b"#DATA:more".to_vec()).await;
        host(&w, b"\x1b[u\x1b[0JStopped\r\n$ ".to_vec()).await;
        until("the prompt", || w.sink.output().ends_with(b"$ ")).await;
        assert_eq!(&w.sink.output()[shown..], b"\x1b[u\x1b[0JStopped\r\n$ ");
    }
}

mod live_sz {
    //! lrzsz's `sz` in a container, through a session's pump. Run with a
    //! container that has lrzsz, `/tmp/a.txt`, and `/tmp/big.bin`:
    //! `TAUMINUS_SZ_CONTAINER=<id> cargo test -p tauminus-app live_sz -- --ignored`.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    use super::transfers::{Attached, Folder, until};
    use super::*;
    use crate::ports::{TransferAnswer, TransferEvent};

    fn container() -> String {
        std::env::var("TAUMINUS_SZ_CONTAINER").expect("TAUMINUS_SZ_CONTAINER names a container")
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a container with lrzsz"]
    async fn files_from_lrzsz_sz_arrive_whole() {
        let id = container();
        let mut child = Command::new("docker")
            .args(["exec", "-i", &id, "sz", "/tmp/a.txt", "/tmp/big.bin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (events_tx, events) = mpsc::channel(64);
        let (input_tx, mut input) = mpsc::channel::<TerminalInput>(64);
        tokio::spawn(async move {
            let mut buffer = vec![0; 32 << 10];
            loop {
                match stdout.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if events_tx
                            .send(TerminalEvent::Output(buffer[..n].to_vec()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            let _ = events_tx
                .send(TerminalEvent::Exited { code: Some(0) })
                .await;
        });
        tokio::spawn(async move {
            while let Some(message) = input.recv().await {
                let TerminalInput::Data(bytes) = message else {
                    continue;
                };
                if stdin.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        let w = Attached::with_stream(input_tx, events);
        until("the offer", || w.host.offers.load(Ordering::Relaxed) == 1).await;
        let folder = Folder::default();
        w.sessions
            .answer_transfer(w.id, 1, TransferAnswer::Receive(Box::new(folder.clone())))
            .unwrap();
        for _ in 0..3000 {
            if matches!(w.host.events().last(), Some(TransferEvent::Finished { .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            matches!(
                w.host.events().last(),
                Some(TransferEvent::Finished { files, .. }) if files.len() == 2
            ),
            "{:?}",
            w.host.events()
        );
        let expected = Command::new("docker")
            .args(["exec", &id, "cat", "/tmp/big.bin"])
            .output()
            .await
            .unwrap()
            .stdout;
        assert_eq!(
            lock(&folder.0).get("big.bin").map(Vec::len),
            Some(expected.len())
        );
        assert!(
            lock(&folder.0)
                .get("big.bin")
                .is_some_and(|got| *got == expected)
        );
        // `sz` ends once it heard the end, and the view saw no file bytes.
        assert!(
            tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .is_ok()
        );
        assert!(w.sink.output().len() < 1000, "{}", w.sink.output().len());
    }
}
