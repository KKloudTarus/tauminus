// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A saved host's tmux session as tabs, through tmux control mode
//! (`tmux -C`). See ADR 0024.
//!
//! One channel with no PTY carries the control client. Each tmux window
//! shows one pane, and each shown pane is a session of its own in the
//! [`SessionManager`], so output batching, flow control, recording,
//! triggers, the prod guard, and the AI assistant work on it unchanged.
//!
//! One task reads the channel and hands each pane's output to that pane's
//! queue without waiting: a pane the view falls behind on never holds up
//! the others or the replies. A queue past [`HIGH`] pauses the pane in tmux
//! (`refresh-client -A %N:pause`: tmux drops its output for this client,
//! and its program goes on); once its view has taken the queue below
//! [`LOW`], it continues and its screen is read again. A pane no tab shows
//! is paused too, so it costs the link nothing. `%N:off`, which stops
//! tmux reading the pane, is never sent: it crashed tmux 3.4 under a flood.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError, Weak};

use tauminus_domain::{
    ControlEvent, ControlParser, Encoding, PaneCursor, PaneId, SavedHost, SessionId, SessionKind,
    SessionMeta, TerminalSize, TmuxCommand, TmuxVersion, WindowId, WindowInfo, layout_panes,
};
use thiserror::Error;
use tokio::sync::{Notify, mpsc, oneshot};
use zeroize::{Zeroize as _, Zeroizing};

use crate::connections::Login;
use crate::ports::{
    ConnectError, OutputSink, Prompter, SinkClosed, SshConnector, TerminalEvent, TerminalInput,
    TerminalStream, Terminator,
};
use crate::session::{SessionError, SessionManager};

/// A pane's queue stops taking output past this, and its screen is read
/// again below [`LOW`].
pub(crate) const HIGH: usize = 1 << 20;
pub(crate) const LOW: usize = 256 << 10;
/// How long a reply to the app's own commands may take.
const ANSWER: std::time::Duration = std::time::Duration::from_secs(15);
/// Replies awaited past this means tmux stopped answering: the client ends.
const MAX_PENDING: usize = 1 << 16;
/// History lines drawn into a pane when it is first shown.
const HISTORY: u32 = 2000;

/// One control client, as the view names it.
pub type ControlId = u64;

#[derive(Debug, Error)]
pub enum TmuxError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("the host names no tmux session to show as tabs")]
    NoSession,
    #[error("tmux windows show as tabs only on a host whose character set is UTF-8")]
    Encoding,
    #[error("tmux {0} is too old to show its windows as tabs: 3.2 or later is needed")]
    Version(String),
    #[error("a window of this tmux session is split into panes, which tabs do not show yet")]
    Split,
    #[error("tmux said no: {0}")]
    Refused(String),
    #[error("the tmux session ended")]
    Ended,
    #[error("tmux did not start on the host: is it installed, and on the login shell's PATH?")]
    NotStarted,
    #[error("no such tmux pane")]
    NoSuchPane,
}

/// A window shown as a tab, and the pane it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabWindow {
    pub window: WindowId,
    pub pane: PaneId,
    pub name: String,
}

/// What changes in the tmux session while it is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopologyEvent {
    /// A window to show, or one whose shown pane changed.
    Window(TabWindow),
    Renamed {
        window: WindowId,
        name: String,
    },
    Closed {
        window: WindowId,
    },
    /// The client ended: detached, the session gone, or the link lost.
    Ended {
        reason: Option<String>,
        lost: bool,
    },
}

/// Where a control client's topology changes go, usually an IPC channel.
pub trait TopologySink: Send + Sync + 'static {
    fn send(&self, event: TopologyEvent) -> Result<(), SinkClosed>;
}

/// A control client just started: its id and the windows it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub control: ControlId,
    pub windows: Vec<TabWindow>,
}

/// Opens tmux sessions as tabs.
pub struct TmuxSessions {
    connector: Arc<dyn SshConnector>,
    sessions: Arc<SessionManager>,
    controls: StdMutex<HashMap<ControlId, Arc<Control>>>,
    next: AtomicU64,
}

impl std::fmt::Debug for TmuxSessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmuxSessions").finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A reply to a command the app sent.
#[derive(Debug)]
struct Reply {
    ok: bool,
    lines: Vec<String>,
}

/// What a reply in the queue is for.
enum Pending {
    Ignore,
    Reply(oneshot::Sender<Reply>),
    /// The windows, again: after one was added, or after `replace`'s shown
    /// pane went from its layout.
    Windows {
        replace: Option<WindowId>,
    },
    Cursor(PaneId),
    Screen(PaneId),
}

enum Outgoing {
    Command(TmuxCommand, Option<oneshot::Sender<Reply>>),
    /// Read a pane's cursor and screen, with this much history, then go on
    /// with its output.
    Capture(PaneId, u32),
}

struct Control {
    outgoing: mpsc::Sender<Outgoing>,
    panes: StdMutex<HashMap<PaneId, Arc<Pane>>>,
    /// The pane each window shows.
    windows: StdMutex<HashMap<WindowId, PaneId>>,
    host: Option<SavedHost>,
    terminator: Box<dyn Terminator>,
    /// A pane was shown: once none is, the client detaches.
    shown: AtomicBool,
    /// The windows were listed at start: listings before that are its own.
    started: AtomicBool,
}

#[derive(Default)]
struct Pane {
    state: StdMutex<PaneState>,
    ready: Notify,
}

#[derive(Default)]
struct PaneState {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    /// Behind: its output is not kept until the view catches up, and then
    /// its screen is read again.
    behind: bool,
    phase: Phase,
    /// How it ended, for its session.
    ended: Option<TerminalEvent>,
    /// Its cursor, read just before its screen.
    cursor: Option<PaneCursor>,
}

#[derive(Default, PartialEq, Eq)]
enum Phase {
    /// No session shows it: its output is not kept.
    #[default]
    Hidden,
    /// Its screen is being read: output before the reply is in it.
    Capturing,
    Live,
}

impl Pane {
    fn end(&self, event: TerminalEvent) {
        let mut state = lock(&self.state);
        if state.ended.is_none() {
            state.ended = Some(event);
        }
        drop(state);
        self.ready.notify_one();
    }
}

impl TmuxSessions {
    pub fn new(connector: Arc<dyn SshConnector>, sessions: Arc<SessionManager>) -> Self {
        Self {
            connector,
            sessions,
            controls: StdMutex::default(),
            next: AtomicU64::new(1),
        }
    }

    /// Joins (or makes) `login`'s tmux session as a control client and
    /// lists its windows. Refused, after detaching, for tmux before 3.2 and
    /// for a session with a split window: the caller joins it the plain way
    /// instead.
    pub async fn open(
        &self,
        login: &Login,
        size: TerminalSize,
        prompter: Arc<dyn Prompter>,
        topology: impl TopologySink,
    ) -> Result<Opened, TmuxError> {
        let tmux = login
            .target
            .session
            .tmux
            .clone()
            .ok_or(TmuxError::NoSession)?;
        if login.target.session.encoding != Encoding::Utf8 {
            return Err(TmuxError::Encoding);
        }
        let connection = self
            .connector
            .connection(
                &login.target,
                prompter,
                Arc::clone(&login.credentials),
                false,
            )
            .await?;
        let stream = connection.open_control(&tmux.control_command()).await?;
        let (outgoing, requests) = mpsc::channel(256);
        let control = Arc::new(Control {
            outgoing,
            panes: StdMutex::default(),
            windows: StdMutex::default(),
            host: Some(login.saved()),
            terminator: stream.terminator,
            shown: AtomicBool::new(false),
            started: AtomicBool::new(false),
        });
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (lines, mut to_write) = mpsc::unbounded_channel::<Zeroizing<String>>();
        let channel = stream.input;
        tokio::spawn(async move {
            // As a secret: the transport wipes it once sent.
            while let Some(line) = to_write.recv().await {
                if channel.send(TerminalInput::Secret(line)).await.is_err() {
                    return;
                }
            }
        });
        tokio::spawn(
            Reader {
                input: lines,
                broken: None,
                parser: ControlParser::default(),
                // The reply to the `new-session` the command line ran.
                fifo: VecDeque::from([Pending::Ignore]),
                control: Arc::downgrade(&control),
                topology: Box::new(topology),
            }
            .run(stream.events, requests),
        );
        let started = match self.start(&control, size).await {
            Err(TmuxError::Ended) => Err(TmuxError::NotStarted),
            other => other,
        };
        match started {
            Ok(windows) => {
                lock(&self.controls).insert(id, control);
                Ok(Opened {
                    control: id,
                    windows,
                })
            }
            Err(error) => {
                let _ = control
                    .outgoing
                    .send(Outgoing::Command(TmuxCommand::Detach, None))
                    .await;
                Err(error)
            }
        }
    }

    async fn start(
        &self,
        control: &Control,
        size: TerminalSize,
    ) -> Result<Vec<TabWindow>, TmuxError> {
        let version = control.ask(TmuxCommand::Version).await?;
        let text = version.first().cloned().unwrap_or_default();
        match TmuxVersion::parse(&text) {
            Some(found) if found >= TmuxVersion::MIN => {}
            _ => return Err(TmuxError::Version(text)),
        }
        let listed: Vec<WindowInfo> = control
            .ask(TmuxCommand::ListWindows)
            .await?
            .iter()
            .filter_map(|line| WindowInfo::parse(line))
            .collect();
        if listed.iter().any(|w| w.panes > 1) {
            return Err(TmuxError::Split);
        }
        control.send(TmuxCommand::Size {
            cols: size.cols(),
            rows: size.rows(),
        });
        let windows: Vec<TabWindow> = listed
            .into_iter()
            .map(|w| TabWindow {
                window: w.window,
                pane: w.pane,
                name: w.name,
            })
            .collect();
        for window in &windows {
            control.add(window);
        }
        control.started.store(true, Ordering::Relaxed);
        Ok(windows)
    }

    /// A live control client; one whose reader ended is forgotten.
    fn control(&self, id: ControlId) -> Result<Arc<Control>, TmuxError> {
        let mut controls = lock(&self.controls);
        // O(C): the ended go whenever any is looked up.
        controls.retain(|_, c| !c.outgoing.is_closed());
        controls.get(&id).cloned().ok_or(TmuxError::Ended)
    }

    /// Shows `pane` in a session of its own, which streams to `sink`: its
    /// screen first, as tmux has it, then what it prints.
    pub async fn attach_pane(
        &self,
        id: ControlId,
        pane: PaneId,
        size: TerminalSize,
        sink: impl OutputSink,
    ) -> Result<SessionId, TmuxError> {
        let control = self.control(id)?;
        let shared = lock(&control.panes)
            .get(&pane)
            .cloned()
            .ok_or(TmuxError::NoSuchPane)?;
        // A slot first: a full table leaves the pane as it was.
        let slot = self.sessions.reserve()?;
        {
            let mut state = lock(&shared.state);
            if state.phase != Phase::Hidden || state.ended.is_some() {
                return Err(TmuxError::NoSuchPane);
            }
            state.phase = Phase::Capturing;
            state.behind = false;
        }
        control.shown.store(true, Ordering::Relaxed);
        let (events_tx, events) = mpsc::channel(64);
        let (input, inputs) = mpsc::channel(64);
        tokio::spawn(forward(
            Arc::clone(&shared),
            events_tx,
            Arc::downgrade(&control),
            pane,
        ));
        tokio::spawn(keys(inputs, Arc::downgrade(&control), pane));
        let session = self.sessions.attach(
            slot,
            TerminalStream {
                input,
                events,
                terminator: Box::new(PaneTerminator {
                    id: pane,
                    pane: Arc::clone(&shared),
                    control: Arc::downgrade(&control),
                }),
            },
            sink,
            SessionMeta {
                kind: SessionKind::Ssh,
                host: control.host.clone(),
                transfers: false,
            },
        );
        control.send(TmuxCommand::Size {
            cols: size.cols(),
            rows: size.rows(),
        });
        // Its output again, then its screen as it is from there.
        let _ = control
            .outgoing
            .send(Outgoing::Command(TmuxCommand::Continue { pane }, None))
            .await;
        let _ = control
            .outgoing
            .send(Outgoing::Capture(pane, HISTORY))
            .await;
        Ok(session)
    }

    /// A new window in the session; it comes back as a topology event.
    pub fn new_window(&self, id: ControlId) -> Result<(), TmuxError> {
        self.control(id)?.send(TmuxCommand::NewWindow);
        Ok(())
    }

    /// Leaves the session running and stops showing it.
    pub fn detach(&self, id: ControlId) -> Result<(), TmuxError> {
        self.control(id)?.send(TmuxCommand::Detach);
        Ok(())
    }
}

impl Control {
    /// Sends a command and waits for its reply.
    async fn ask(&self, command: TmuxCommand) -> Result<Vec<String>, TmuxError> {
        let (tx, rx) = oneshot::channel();
        self.outgoing
            .send(Outgoing::Command(command, Some(tx)))
            .await
            .map_err(|_| TmuxError::Ended)?;
        let reply = tokio::time::timeout(ANSWER, rx)
            .await
            .map_err(|_| TmuxError::Refused("tmux did not answer".into()))?
            .map_err(|_| TmuxError::Ended)?;
        if reply.ok {
            Ok(reply.lines)
        } else {
            Err(TmuxError::Refused(reply.lines.join(" ")))
        }
    }

    /// Sends a command whose reply does not matter; dropped if the queue is
    /// full, which only a stalled channel fills.
    fn send(&self, command: TmuxCommand) {
        let _ = self.outgoing.try_send(Outgoing::Command(command, None));
    }

    /// Starts tracking `window`'s pane, not shown yet: paused, so its
    /// output does not cross the link until a tab shows it.
    fn add(&self, window: &TabWindow) {
        lock(&self.windows).insert(window.window, window.pane);
        lock(&self.panes).entry(window.pane).or_default();
        self.send(TmuxCommand::Pause { pane: window.pane });
    }

    /// Whether any pane is shown; the client detaches when the last is
    /// closed.
    fn shows_any(&self) -> bool {
        lock(&self.panes)
            .values()
            .any(|p| lock(&p.state).phase != Phase::Hidden)
    }
}

/// Reads the control channel and writes commands to it.
struct Reader {
    /// To the writer task, in order: the reader never waits on a write,
    /// which would wait on the reader. Bounded with the replies awaited, at
    /// [`MAX_PENDING`]. Lines are wiped once written: one may carry keys.
    input: mpsc::UnboundedSender<Zeroizing<String>>,
    /// Why the client must end, once it must.
    broken: Option<&'static str>,
    parser: ControlParser,
    fifo: VecDeque<Pending>,
    control: Weak<Control>,
    topology: Box<dyn TopologySink>,
}

impl Reader {
    async fn run(
        mut self,
        mut events: mpsc::Receiver<TerminalEvent>,
        mut requests: mpsc::Receiver<Outgoing>,
    ) {
        let (reason, lost) = 'run: loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(TerminalEvent::Output(bytes)) => {
                        let parsed = match self.parser.push(&bytes) {
                            Ok(parsed) => parsed,
                            Err(error) => {
                                tracing::warn!(%error, "tmux control stream unreadable");
                                break 'run (Some(error.to_string()), false);
                            }
                        };
                        for event in parsed {
                            if let Some(ended) = self.handle(event) {
                                break 'run ended;
                            }
                        }
                        if let Some(why) = self.broken {
                            break 'run (Some(why.to_owned()), false);
                        }
                    }
                    Some(TerminalEvent::Exited { .. }) | None => break (None, false),
                    Some(TerminalEvent::Lost) => break (None, true),
                },
                request = requests.recv() => match request {
                    Some(request) => {
                        self.write(request);
                        if let Some(why) = self.broken {
                            break 'run (Some(why.to_owned()), false);
                        }
                    }
                    None => break (None, false),
                },
            }
        };
        self.finish(reason, lost);
    }

    fn line(&mut self, line: String, pending: Pending) {
        let line = Zeroizing::new(line);
        if self.fifo.len() >= MAX_PENDING {
            self.broken = Some("tmux stopped answering");
            return;
        }
        self.fifo.push_back(pending);
        let _ = self.input.send(line);
    }

    fn write(&mut self, request: Outgoing) {
        match request {
            Outgoing::Command(mut command, mut reply) => {
                let encoded = command.encode();
                if let TmuxCommand::Keys { bytes, .. } = &mut command {
                    bytes.zeroize();
                }
                let mut lines = encoded.into_iter().peekable();
                while let Some(line) = lines.next() {
                    // One reply per line; the waiter takes the last one's.
                    let pending = match (lines.peek(), reply.take()) {
                        (None, Some(reply)) => Pending::Reply(reply),
                        (_, taken) => {
                            reply = taken;
                            Pending::Ignore
                        }
                    };
                    self.line(line, pending);
                }
            }
            // One line, two replies: the cursor's, then the screen's.
            Outgoing::Capture(pane, history) => {
                for line in (TmuxCommand::Snapshot { pane, history }).encode() {
                    self.line(line, Pending::Cursor(pane));
                    if self.broken.is_none() {
                        self.fifo.push_back(Pending::Screen(pane));
                    }
                }
            }
        }
    }

    /// `Some` when the client ended: why, and whether the link was lost.
    fn handle(&mut self, event: ControlEvent) -> Option<(Option<String>, bool)> {
        let control = self.control.upgrade()?;
        match event {
            ControlEvent::Reply { ok, lines, .. } => match self.fifo.pop_front() {
                Some(Pending::Reply(tx)) => {
                    let _ = tx.send(Reply { ok, lines });
                }
                // Before start's own listing, a window tmux announced is in it.
                Some(Pending::Windows { replace })
                    if ok && control.started.load(Ordering::Relaxed) =>
                {
                    self.windows(&control, &lines, replace);
                }
                Some(Pending::Cursor(pane)) => {
                    if let Some(shared) = lock(&control.panes).get(&pane) {
                        lock(&shared.state).cursor =
                            lines.first().and_then(|l| PaneCursor::parse(l));
                    }
                }
                Some(Pending::Screen(pane)) => {
                    if let Some(shared) = lock(&control.panes).get(&pane).cloned() {
                        let mut state = lock(&shared.state);
                        let cursor = state.cursor.take();
                        // A failed read (the pane gone) is no screen.
                        if state.phase == Phase::Capturing && ok {
                            // Not counted against the pane: a screen with
                            // much history is no sign of a flood.
                            state.queue.push_back(redraw(&lines, cursor));
                            state.phase = Phase::Live;
                        }
                        drop(state);
                        shared.ready.notify_one();
                    }
                }
                Some(Pending::Ignore | Pending::Windows { .. }) | None => {}
            },
            ControlEvent::Output { pane, data } => {
                if let Some(shared) = lock(&control.panes).get(&pane).cloned() {
                    let mut state = lock(&shared.state);
                    // Behind: dropped; its screen is read again once caught up.
                    let mut pause = false;
                    if state.phase == Phase::Live && state.ended.is_none() && !state.behind {
                        if state.bytes + data.len() > HIGH {
                            state.behind = true;
                            pause = true;
                        } else {
                            state.bytes += data.len();
                            state.queue.push_back(data);
                        }
                    }
                    drop(state);
                    shared.ready.notify_one();
                    if pause {
                        self.command(TmuxCommand::Pause { pane });
                    }
                }
            }
            ControlEvent::WindowAdd { .. } => self.list_windows(None),
            ControlEvent::LayoutChange { window, layout } => {
                let shown = lock(&control.windows).get(&window).copied();
                if shown.is_some_and(|pane| !layout_panes(&layout).contains(&pane)) {
                    self.list_windows(Some(window));
                }
            }
            ControlEvent::WindowClose { window } => {
                if let Some(pane) = lock(&control.windows).remove(&window)
                    && let Some(shared) = lock(&control.panes).remove(&pane)
                {
                    shared.end(TerminalEvent::Exited { code: None });
                }
                let _ = self.topology.send(TopologyEvent::Closed { window });
                self.detach_if_unseen(&control);
            }
            ControlEvent::WindowRenamed { window, name } => {
                let _ = self.topology.send(TopologyEvent::Renamed { window, name });
            }
            ControlEvent::Exit { reason } => return Some((reason, false)),
            // tmux saying so of a pause the app asked for.
            ControlEvent::Pause { .. }
            | ControlEvent::Continue { .. }
            | ControlEvent::SessionChanged { .. } => {}
        }
        None
    }

    fn command(&mut self, command: TmuxCommand) {
        self.write(Outgoing::Command(command, None));
    }

    /// Once a pane was shown and none is now, the client goes: it would
    /// hold window sizes, and no tab is left to detach it from.
    fn detach_if_unseen(&mut self, control: &Control) {
        if control.shown.load(Ordering::Relaxed) && !control.shows_any() {
            self.command(TmuxCommand::Detach);
        }
    }

    fn list_windows(&mut self, replace: Option<WindowId>) {
        for line in TmuxCommand::ListWindows.encode() {
            self.line(line, Pending::Windows { replace });
        }
    }

    /// The windows as listed now: new ones are shown, and `replace` shows
    /// the pane it has now, its old one having gone.
    fn windows(&mut self, control: &Control, lines: &[String], replace: Option<WindowId>) {
        for info in lines.iter().filter_map(|l| WindowInfo::parse(l)) {
            let known = lock(&control.windows).get(&info.window).copied();
            match known {
                None => {}
                Some(pane) if replace == Some(info.window) && pane != info.pane => {
                    if let Some(old) = lock(&control.panes).remove(&pane) {
                        old.end(TerminalEvent::Exited { code: None });
                    }
                }
                Some(_) => continue,
            }
            let window = TabWindow {
                window: info.window,
                pane: info.pane,
                name: info.name,
            };
            control.add(&window);
            let _ = self.topology.send(TopologyEvent::Window(window));
        }
    }

    fn finish(self, reason: Option<String>, lost: bool) {
        if let Some(control) = self.control.upgrade() {
            for (_, pane) in lock(&control.panes).drain() {
                pane.end(if lost {
                    TerminalEvent::Lost
                } else {
                    TerminalEvent::Exited { code: None }
                });
            }
            control.terminator.terminate();
        }
        // Waiting askers learn of the end as their replies drop.
        let _ = self.topology.send(TopologyEvent::Ended { reason, lost });
    }
}

/// The screen tmux read for a pane, drawn on a fresh terminal: its rows
/// (history scrolls up as it goes), then the cursor where it was.
fn redraw(rows: &[String], cursor: Option<PaneCursor>) -> Vec<u8> {
    // The modes as tmux has them, whatever output was dropped: first all
    // off (normal screen, whole-screen scrolling, no mouse, normal keys),
    // then those the pane's program asked for.
    let mut out = b"\x1b[?1049l\x1b[r\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l".to_vec();
    if let Some(c) = cursor {
        for (on, set) in [
            (c.alternate, &b"\x1b[?1049h"[..]),
            (c.keypad, b"\x1b[?1h"),
            (c.mouse_standard, b"\x1b[?1000h"),
            (c.mouse_button, b"\x1b[?1002h"),
            (c.mouse_any, b"\x1b[?1003h"),
            (c.mouse_sgr, b"\x1b[?1006h"),
        ] {
            if on {
                out.extend_from_slice(set);
            }
        }
    }
    out.extend_from_slice(b"\x1b[H\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
        out.extend_from_slice(row.as_bytes());
    }
    out.extend_from_slice(b"\x1b[0m");
    if let Some(cursor) = cursor {
        out.extend_from_slice(
            format!(
                "\x1b[{};{}H",
                u32::from(cursor.y) + 1,
                u32::from(cursor.x) + 1
            )
            .as_bytes(),
        );
    }
    out
}

/// Moves a pane's queue to its session, one chunk at a time, turning its
/// output on again once the queue is short.
async fn forward(
    pane: Arc<Pane>,
    events: mpsc::Sender<TerminalEvent>,
    control: Weak<Control>,
    id: PaneId,
) {
    loop {
        let (chunk, resume, ended) = {
            let mut state = lock(&pane.state);
            // Its session closed: the stream ends with it.
            if state.phase == Phase::Hidden {
                return;
            }
            let chunk = state.queue.pop_front();
            if let Some(chunk) = &chunk {
                state.bytes = state.bytes.saturating_sub(chunk.len());
            }
            // Caught up with what was kept: read the screen as tmux has it
            // now, and go on from there.
            let resume = state.behind && state.bytes < LOW;
            if resume {
                state.behind = false;
                state.phase = Phase::Capturing;
            }
            let ended = if chunk.is_none() {
                state.ended.clone()
            } else {
                None
            };
            (chunk, resume, ended)
        };
        if resume && let Some(control) = control.upgrade() {
            let _ = control
                .outgoing
                .send(Outgoing::Command(TmuxCommand::Continue { pane: id }, None))
                .await;
            let _ = control.outgoing.send(Outgoing::Capture(id, 0)).await;
        }
        if let Some(chunk) = chunk {
            if events.send(TerminalEvent::Output(chunk)).await.is_err() {
                return;
            }
            continue;
        }
        if let Some(ended) = ended {
            let _ = events.send(ended).await;
            return;
        }
        pane.ready.notified().await;
    }
}

/// Sends what is typed in a pane's session as keys to its tmux pane.
async fn keys(mut inputs: mpsc::Receiver<TerminalInput>, control: Weak<Control>, pane: PaneId) {
    while let Some(input) = inputs.recv().await {
        let Some(control) = control.upgrade() else {
            return;
        };
        let command = match input {
            TerminalInput::Text(text) => TmuxCommand::Keys {
                pane,
                bytes: text.into_bytes(),
            },
            TerminalInput::Data(bytes) => TmuxCommand::Keys { pane, bytes },
            // Hex on a command line, like any key: the bytes are wiped once
            // encoded, and the line once written.
            TerminalInput::Secret(secret) => TmuxCommand::Keys {
                pane,
                bytes: secret.as_bytes().to_vec(),
            },
            TerminalInput::Resize(size) => TmuxCommand::Size {
                cols: size.cols(),
                rows: size.rows(),
            },
        };
        let _ = control
            .outgoing
            .send(Outgoing::Command(command, None))
            .await;
    }
}

/// Closing a pane's session stops showing it; the window goes on in tmux.
/// The client detaches when no pane is shown.
struct PaneTerminator {
    id: PaneId,
    pane: Arc<Pane>,
    control: Weak<Control>,
}

impl Terminator for PaneTerminator {
    fn terminate(&self) {
        {
            let mut state = lock(&self.pane.state);
            state.phase = Phase::Hidden;
            state.queue.clear();
            state.bytes = 0;
            state.behind = false;
        }
        self.pane.ready.notify_one();
        if let Some(control) = self.control.upgrade() {
            if control.shows_any() {
                control.send(TmuxCommand::Pause { pane: self.id });
            } else {
                control.send(TmuxCommand::Detach);
            }
        }
    }

    fn probe(&self) {
        if let Some(control) = self.control.upgrade() {
            control.terminator.probe();
        }
    }
}

#[cfg(test)]
mod tests;
