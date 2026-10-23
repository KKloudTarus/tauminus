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
