// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Against a fake tmux that answers control mode commands as tmux does.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tauminus_domain::{SshTarget, TmuxSession};

use super::*;
use crate::SessionConfig;
use crate::fakes::{login, prompter};
use crate::ports::{
    BoxFuture, Credentials, ForwardHandle, ScriptOutcome, SftpPort, SshConnection, TunnelError,
};

/// What the fake tmux holds, and what it was sent.
#[derive(Default)]
struct State {
    version: String,
    /// `@W %P PANES NAME`.
    windows: Vec<String>,
    screens: HashMap<u32, Vec<String>>,
    keys: HashMap<u32, Vec<u8>>,
    commands: Vec<String>,
    /// Printed just before a capture's reply: output the capture holds.
    before_capture: Vec<u8>,
    /// What the login shell prints before tmux starts, with no line break.
    greeting: Vec<u8>,
    /// No tmux on the host: the command ends at once.
    absent: bool,
    /// tmux announces the session's window before the app lists them.
    early_window: bool,
    to_app: Option<mpsc::Sender<TerminalEvent>>,
    next_window: u32,
}

#[derive(Clone, Default)]
struct FakeTmux(Arc<StdMutex<State>>);

impl FakeTmux {
    fn new(version: &str, windows: &[&str]) -> Self {
        let tmux = Self::default();
        {
            let mut state = lock(&tmux.0);
            state.version = version.to_owned();
            state.windows = windows.iter().map(|w| (*w).to_owned()).collect();
            state.next_window = 10;
        }
        tmux
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.0)
    }

    fn commands(&self) -> Vec<String> {
        self.state().commands.clone()
    }

    async fn emit(&self, text: &[u8]) {
        let to_app = self.state().to_app.clone();
        if let Some(to_app) = to_app {
            let _ = to_app.send(TerminalEvent::Output(text.to_vec())).await;
        }
    }

    async fn serve(self, mut inputs: mpsc::Receiver<TerminalInput>) {
        if self.state().absent {
            self.state().to_app = None;
            return;
        }
        // The reply to the command line's `new-session`.
        let mut first = self.state().greeting.clone();
        first.extend_from_slice(b"%begin 1 0 1\n%end 1 0 1\n");
        if self.state().early_window {
            first.extend_from_slice(b"%window-add @1\n");
        }
        self.emit(&first).await;
        let mut number = 1;
        while let Some(input) = inputs.recv().await {
            let text = match input {
                TerminalInput::Data(bytes) => String::from_utf8(bytes).unwrap(),
                TerminalInput::Secret(secret) => secret.as_str().to_owned(),
                _ => continue,
            };
            // `a ; b` is two commands, each with its reply.
            for line in text.lines().flat_map(|l| l.split(" ; ")) {
                number += 1;
                let (lines, ok, after) = self.answer(line);
                let mut out = Vec::new();
                if line.starts_with("capture-pane") {
                    out.extend(self.state().before_capture.clone());
                }
                out.extend(format!("%begin 1 {number} 1\n").into_bytes());
                for l in &lines {
                    out.extend(format!("{l}\n").into_bytes());
                }
                let guard = if ok { "%end" } else { "%error" };
                out.extend(format!("{guard} 1 {number} 1\n").into_bytes());
                out.extend(after.into_bytes());
                self.emit(&out).await;
                if line == "detach-client" {
                    self.emit(b"%exit\n").await;
                    self.state().to_app = None;
                    return;
                }
            }
        }
    }

    /// The reply's lines, whether it is `%end`, and what tmux prints after.
    fn answer(&self, line: &str) -> (Vec<String>, bool, String) {
        let mut state = self.state();
        state.commands.push(line.to_owned());
        let pane = |line: &str| -> u32 {
            line.split(" -t %")
                .nth(1)
                .and_then(|r| r.split(' ').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0)
        };
        if line == "display-message -p '#{version}'" {
            return (vec![state.version.clone()], true, String::new());
        }
        if line.starts_with("list-windows") {
            return (state.windows.clone(), true, String::new());
        }
        if line.starts_with("capture-pane") {
            let screen = state.screens.get(&pane(line)).cloned().unwrap_or_default();
            return (screen, true, String::new());
        }
        if line.starts_with("display-message -p -t %") {
            return (vec!["3 1 0".into()], true, String::new());
        }
        if line.starts_with("send-keys -H -t %") {
            let target = pane(line);
            let bytes: Vec<u8> = line
                .split(' ')
                .skip(4)
                .map(|h| u8::from_str_radix(h, 16).unwrap())
                .collect();
            state.keys.entry(target).or_default().extend(bytes);
            return (vec![], true, String::new());
        }
        if line == "new-window" {
            let window = state.next_window;
            state.next_window += 1;
            state.windows.push(format!("@{window} %{window} 1 new"));
            return (vec![], true, format!("%window-add @{window}\n"));
        }
        if line.starts_with("refresh-client") || line == "detach-client" {
            return (vec![], true, String::new());
        }
        (vec!["unknown command".into()], false, String::new())
    }
}

struct FakeConnection(FakeTmux);

struct Ends(FakeTmux);

impl Terminator for Ends {
    fn terminate(&self) {
        self.0.state().to_app = None;
    }
}

impl SshConnection for FakeConnection {
    fn open_shell<'a>(
        &'a self,
        _size: TerminalSize,
        _options: &'a tauminus_domain::SessionOptions,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn open_control<'a>(
        &'a self,
        command: &'a str,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        assert!(
            command.contains("tmux -u -C new-session -A -s work"),
            "{command}"
        );
        let tmux = self.0.clone();
        Box::pin(async move {
            let (to_app, events) = mpsc::channel(64);
            let (input, inputs) = mpsc::channel(64);
            tmux.state().to_app = Some(to_app);
            tokio::spawn(tmux.clone().serve(inputs));
            Ok(TerminalStream {
                input,
                events,
                terminator: Box::new(Ends(tmux)),
            })
        })
    }

    fn start_forward<'a>(
        &'a self,
        _forward: &'a tauminus_domain::Forward,
    ) -> BoxFuture<'a, Result<ForwardHandle, TunnelError>> {
        Box::pin(async { Err(TunnelError::Closed) })
    }

    fn open_sftp(&self) -> BoxFuture<'_, Result<Arc<dyn SftpPort>, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn is_open(&self) -> bool {
        true
    }

    fn closed(&self) -> BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}

struct Connector(FakeTmux);

impl SshConnector for Connector {
    fn connection<'a>(
        &'a self,
        _target: &'a SshTarget,
        _prompter: Arc<dyn Prompter>,
        _credentials: Arc<Credentials>,
        _forward_agent: bool,
    ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>> {
        let tmux = self.0.clone();
        Box::pin(async move { Ok(Arc::new(FakeConnection(tmux)) as Arc<dyn SshConnection>) })
    }

    fn connect_with<'a>(
        &'a self,
        _target: &'a SshTarget,
        _size: TerminalSize,
        _prompter: Arc<dyn Prompter>,
        _credentials: Arc<Credentials>,
    ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn run_script<'a>(
        &'a self,
        _target: &'a SshTarget,
        _prompter: Arc<dyn Prompter>,
        _credentials: Arc<Credentials>,
        _script: &'a str,
    ) -> BoxFuture<'a, Result<ScriptOutcome, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }
}

#[derive(Clone, Default)]
struct Topology(Arc<StdMutex<Vec<TopologyEvent>>>);

impl TopologySink for Topology {
    fn send(&self, event: TopologyEvent) -> Result<(), SinkClosed> {
        lock(&self.0).push(event);
        Ok(())
    }
}

impl Topology {
    fn events(&self) -> Vec<TopologyEvent> {
        lock(&self.0).clone()
    }
}

#[derive(Clone, Default)]
struct Sink {
    output: Arc<StdMutex<Vec<u8>>>,
    exits: Arc<StdMutex<Vec<Option<u32>>>>,
}

impl OutputSink for Sink {
    fn send_output(&self, batch: Vec<u8>) -> Result<(), SinkClosed> {
        lock(&self.output).extend(batch);
        Ok(())
    }

    fn send_exit(&self, code: Option<u32>) {
        lock(&self.exits).push(code);
    }
}

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&lock(&self.output)).into_owned()
    }
}

struct World {
    tmux: FakeTmux,
    tmuxes: TmuxSessions,
    sessions: Arc<SessionManager>,
    topology: Topology,
}

fn world(version: &str, windows: &[&str]) -> World {
    let tmux = FakeTmux::new(version, windows);
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    World {
        tmuxes: TmuxSessions::new(Arc::new(Connector(tmux.clone())), Arc::clone(&sessions)),
        tmux,
        sessions,
        topology: Topology::default(),
    }
}

fn work_login() -> Login {
    let mut login = login("db", "db.example");
    login.target.session.tmux = Some(TmuxSession::parse("work").unwrap());
    login
}

impl World {
    async fn open(&self) -> Result<Opened, TmuxError> {
        self.tmuxes
            .open(
                &work_login(),
                TerminalSize::new(100, 30).unwrap(),
                prompter(),
                self.topology.clone(),
            )
            .await
    }
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn opens_with_the_session_s_windows_and_its_size() {
    let world = world("3.4", &["@1 %1 1 shell", "@2 %5 1 vim notes.md"]);
    let opened = world.open().await.unwrap();
    assert_eq!(
        opened.windows,
        vec![
            TabWindow {
                window: WindowId(1),
                pane: PaneId(1),
                name: "shell".into()
            },
            TabWindow {
                window: WindowId(2),
                pane: PaneId(5),
                name: "vim notes.md".into()
            },
        ]
    );
    until("the size", || {
        world
            .tmux
            .commands()
            .contains(&"refresh-client -C 100x30".to_owned())
    })
    .await;
    // No tab shows them yet: their output does not cross the link.
    until("the pauses", || {
        let commands = world.tmux.commands();
        ["%1", "%5"]
            .iter()
            .all(|p| commands.contains(&format!("refresh-client -A '{p}:pause'")))
    })
    .await;
}

#[tokio::test]
async fn refuses_old_tmux_and_split_windows_and_detaches() {
    for (version, windows, refused) in [
        ("3.1c", vec!["@1 %1 1 a"], "Version"),
        ("2.7", vec!["@1 %1 1 a"], "Version"),
        ("master", vec!["@1 %1 1 a"], "Version"),
        ("3.3a", vec!["@1 %1 1 a", "@2 %2 3 split"], "Split"),
    ] {
        let world = world(version, &windows);
        let error = world.open().await.unwrap_err();
        assert!(
            format!("{error:?}").starts_with(refused),
            "{version}: {error:?}"
        );
        until("the detach", || {
            world.tmux.commands().contains(&"detach-client".to_owned())
        })
        .await;
    }
}

#[tokio::test]
async fn a_shown_pane_draws_its_screen_then_streams_and_takes_keys() {
    let world = world("3.2a", &["@1 %1 1 shell"]);
    world
        .tmux
        .state()
        .screens
        .insert(1, vec!["$ ls".into(), "a b".into(), "$ ".into()]);
    // Printed before the capture's reply: already on the captured screen.
    world.tmux.state().before_capture = b"%output %1 OLD\n".to_vec();
    let opened = world.open().await.unwrap();
    let sink = Sink::default();
    let id = world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::new(100, 30).unwrap(),
            sink.clone(),
        )
        .await
        .unwrap();
    until("the screen", || sink.text().contains("a b")).await;
    assert!(sink.text().contains("$ ls\x1b[0m\r\na b\x1b[0m\r\n$ "));
    // The modes as tmux has them: none set by the program here.
    assert!(sink.text().contains("\x1b[?1049l\x1b[r"));
    assert!(!sink.text().contains("\x1b[?1049h"));
    assert!(
        sink.text().ends_with("\x1b[2;4H"),
        "cursor at 3,1: {:?}",
        sink.text()
    );
    assert!(!sink.text().contains("OLD"));
    world.tmux.emit(b"%output %1 hi\\015\\012\n").await;
    until("the output", || sink.text().ends_with("hi\r\n")).await;
    world.sessions.write(id, "pwd\r".into()).await.unwrap();
    until("the keys", || {
        world
            .tmux
            .state()
            .keys
            .get(&1)
            .is_some_and(|k| k == b"pwd\r")
    })
    .await;
    // The meta is the saved host's, so the guard and the AI apply as on SSH.
    assert_eq!(world.sessions.meta(id).unwrap().kind, SessionKind::Ssh);
    assert!(world.sessions.meta(id).unwrap().host.is_some());
}

#[tokio::test]
async fn windows_added_closed_renamed_and_replaced_reach_the_view() {
    let world = world("3.4", &["@1 %1 1 shell"]);
    let opened = world.open().await.unwrap();
    let sink = Sink::default();
    world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            sink.clone(),
        )
        .await
        .unwrap();
    world.tmuxes.new_window(opened.control).unwrap();
    until("the new window", || {
        world
            .topology
            .events()
            .contains(&TopologyEvent::Window(TabWindow {
                window: WindowId(10),
                pane: PaneId(10),
                name: "new".into(),
            }))
    })
    .await;
    world.tmux.emit(b"%window-renamed @10 logs\n").await;
    // Pane %1 went from window @1's layout; the window shows %3 now.
    world.tmux.state().windows[0] = "@1 %3 1 shell".into();
    world
        .tmux
        .emit(b"%layout-change @1 b25f,100x30,0,0,3 b25f,100x30,0,0,3 *\n")
        .await;
    until("the replaced pane", || {
        world
            .topology
            .events()
            .contains(&TopologyEvent::Window(TabWindow {
                window: WindowId(1),
                pane: PaneId(3),
                name: "shell".into(),
            }))
    })
    .await;
    until("the old pane's end", || !lock(&sink.exits).is_empty()).await;
    world.tmux.emit(b"%window-close @10\n").await;
    until("the close", || {
        world.topology.events().contains(&TopologyEvent::Closed {
            window: WindowId(10),
        })
    })
    .await;
    assert!(world.topology.events().contains(&TopologyEvent::Renamed {
        window: WindowId(10),
        name: "logs".into()
    }));
}

#[tokio::test]
async fn a_pane_the_view_falls_behind_on_skips_to_its_screen_and_holds_no_other_up() {
    let world = world("3.4", &["@1 %1 1 flood", "@2 %2 1 quiet"]);
    let opened = world.open().await.unwrap();
    let flood = Sink::default();
    let quiet = Sink::default();
    let id = world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            flood.clone(),
        )
        .await
        .unwrap();
    world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(2),
            TerminalSize::default(),
            quiet.clone(),
        )
        .await
        .unwrap();
    // Not acknowledged yet: the flooded session stops taking output, and
    // the pane's queue stops at its cap.
    let line = format!("%output %1 {}\n", "x".repeat(16 * 1024));
    for _ in 0..400 {
        world.tmux.emit(line.as_bytes()).await;
    }
    world.tmux.emit(b"%output %2 still here\n").await;
    until("the other pane", || quiet.text().contains("still here")).await;
    // Paused, never turned off: `off` crashed tmux 3.4.
    until("the pause", || {
        world
            .tmux
            .commands()
            .contains(&"refresh-client -A '%1:pause'".to_owned())
    })
    .await;
    assert!(!world.tmux.commands().iter().any(|c| c.contains(":off")));
    // The view catches up: the pane's screen is read again, no history.
    world.tmux.state().screens.insert(1, vec!["$ after".into()]);
    for _ in 0..200 {
        world.sessions.acknowledge(id, 1 << 20).unwrap();
        if world
            .tmux
            .commands()
            .iter()
            .any(|c| c.contains("capture-pane -p -e -t %1 -S -0"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    for _ in 0..400 {
        if flood.text().ends_with("$ after\x1b[0m\x1b[2;4H") {
            assert!(
                world
                    .tmux
                    .commands()
                    .iter()
                    .filter(|c| *c == "refresh-client -A '%1:continue'")
                    .count()
                    >= 2,
                "continued when shown, and again once caught up"
            );
            return;
        }
        world.sessions.acknowledge(id, 1 << 20).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let text = flood.text();
    panic!(
        "never: the screen again; tail {:?}",
        &text[text.len().saturating_sub(80)..]
    );
}

#[tokio::test]
async fn a_window_announced_before_the_start_lists_them_opens_once() {
    let world = world("3.4", &["@1 %1 1 shell"]);
    world.tmux.state().early_window = true;
    let opened = world.open().await.unwrap();
    assert_eq!(opened.windows.len(), 1);
    // The announcement's own listing comes and goes without an event.
    until("its listing", || {
        world
            .tmux
            .commands()
            .iter()
            .filter(|c| c.starts_with("list-windows"))
            .count()
            >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !world
            .topology
            .events()
            .iter()
            .any(|e| matches!(e, TopologyEvent::Window(_)))
    );
}

#[tokio::test]
async fn closing_one_of_two_tabs_pauses_its_pane_and_keeps_the_client() {
    let world = world("3.4", &["@1 %1 1 a", "@2 %2 1 b"]);
    let opened = world.open().await.unwrap();
    let one = world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            Sink::default(),
        )
        .await
        .unwrap();
    world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(2),
            TerminalSize::default(),
            Sink::default(),
        )
        .await
        .unwrap();
    let pauses = || {
        world
            .tmux
            .commands()
            .iter()
            .filter(|c| *c == "refresh-client -A '%1:pause'")
            .count()
    };
    let before = pauses();
    world.sessions.close(one).unwrap();
    until("the pause", || pauses() > before).await;
    assert!(!world.tmux.commands().contains(&"detach-client".to_owned()));
}

#[tokio::test]
async fn the_login_shell_s_greeting_before_tmux_is_skipped() {
    let world = world("3.4", &["@1 %1 1 shell"]);
    world.tmux.state().greeting = b"Welcome to db".to_vec();
    let opened = world.open().await.unwrap();
    assert_eq!(opened.windows.len(), 1);
}

#[tokio::test]
async fn a_host_without_tmux_is_refused_so_it_joins_the_plain_way() {
    let world = world("3.4", &["@1 %1 1 shell"]);
    world.tmux.state().absent = true;
    assert!(matches!(world.open().await, Err(TmuxError::NotStarted)));
}

#[tokio::test]
async fn an_attach_the_session_table_refuses_leaves_the_pane_to_show_later() {
    let tmux = FakeTmux::new("3.4", &["@1 %1 1 a", "@2 %2 1 b"]);
    let sessions = Arc::new(SessionManager::new(SessionConfig {
        max_sessions: 1,
        ..SessionConfig::default()
    }));
    let world = World {
        tmuxes: TmuxSessions::new(Arc::new(Connector(tmux.clone())), Arc::clone(&sessions)),
        tmux,
        sessions,
        topology: Topology::default(),
    };
    let opened = world.open().await.unwrap();
    let first = world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            Sink::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        world
            .tmuxes
            .attach_pane(
                opened.control,
                PaneId(2),
                TerminalSize::default(),
                Sink::default()
            )
            .await,
        Err(TmuxError::Session(_))
    ));
    world.sessions.close(first).unwrap();
    // No pane is left half shown: with no tab open, the client detaches.
    until("the detach", || {
        world.tmux.commands().contains(&"detach-client".to_owned())
    })
    .await;
}

#[tokio::test]
async fn the_client_detaches_once_the_last_shown_window_exits() {
    let world = world("3.4", &["@1 %1 1 shown", "@2 %2 1 hidden"]);
    let opened = world.open().await.unwrap();
    world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            Sink::default(),
        )
        .await
        .unwrap();
    world.tmux.emit(b"%window-close @1\n").await;
    until("the detach", || {
        world.tmux.commands().contains(&"detach-client".to_owned())
    })
    .await;
}

#[tokio::test]
async fn closing_the_last_shown_tab_detaches_and_the_end_reaches_all() {
    let world = world("3.4", &["@1 %1 1 shell"]);
    let opened = world.open().await.unwrap();
    let sink = Sink::default();
    let id = world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            sink.clone(),
        )
        .await
        .unwrap();
    world.sessions.close(id).unwrap();
    until("the detach", || {
        world.tmux.commands().contains(&"detach-client".to_owned())
    })
    .await;
    until("the end", || {
        world.topology.events().contains(&TopologyEvent::Ended {
            reason: None,
            lost: false,
        })
    })
    .await;
    assert!(matches!(
        world.tmuxes.new_window(opened.control),
        Err(TmuxError::Ended)
    ));
}

#[tokio::test]
async fn a_lost_link_ends_every_pane_as_lost() {
    let world = world("3.4", &["@1 %1 1 shell"]);
    let opened = world.open().await.unwrap();
    let sink = Sink::default();
    world
        .tmuxes
        .attach_pane(
            opened.control,
            PaneId(1),
            TerminalSize::default(),
            sink.clone(),
        )
        .await
        .unwrap();
    let to_app = world.tmux.state().to_app.clone().unwrap();
    to_app.send(TerminalEvent::Lost).await.unwrap();
    until("the end", || {
        world.topology.events().contains(&TopologyEvent::Ended {
            reason: None,
            lost: true,
        })
    })
    .await;
}

#[tokio::test]
async fn a_host_without_a_tmux_session_or_utf_8_is_refused() {
    let world = world("3.4", &[]);
    let mut plain = login("db", "db.example");
    let error = world
        .tmuxes
        .open(
            &plain,
            TerminalSize::default(),
            prompter(),
            Topology::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, TmuxError::NoSession));
    plain.target.session.tmux = Some(TmuxSession::parse("work").unwrap());
    plain.target.session.encoding = Encoding::parse("euc-kr").unwrap();
    let error = world
        .tmuxes
        .open(
            &plain,
            TerminalSize::default(),
            prompter(),
            Topology::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, TmuxError::Encoding));
}

/// A real tmux on a socket of its own (`-L`), never the default one, with
/// no config file: `cargo test -p tauminus-app -- --ignored real_tmux`.
mod real {
    use std::process::Stdio;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::process::{Child, Command};

    use super::*;

    struct Local {
        socket: String,
    }

    struct Kill(StdMutex<Option<Child>>);

    impl Terminator for Kill {
        fn terminate(&self) {
            if let Some(child) = lock(&self.0).as_mut() {
                let _ = child.start_kill();
            }
        }
    }

    impl SshConnection for Local {
        fn open_shell<'a>(
            &'a self,
            _size: TerminalSize,
            _options: &'a tauminus_domain::SessionOptions,
        ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
            Box::pin(async { Err(ConnectError::Cancelled) })
        }

        fn open_control<'a>(
            &'a self,
            _command: &'a str,
        ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
            Box::pin(async move {
                let mut child = Command::new("tmux")
                    .args(["-L", &self.socket, "-f", "/dev/null", "-u", "-C"])
                    .args(["new-session", "-A", "-s", "work", "-x", "100", "-y", "30"])
                    .env_remove("TMUX")
                    .env("TMUX_TMPDIR", dir(&self.socket))
                    .env("SHELL", "/bin/sh")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let mut stdin = child.stdin.take().unwrap();
                let mut stdout = child.stdout.take().unwrap();
                let (to_app, events) = mpsc::channel(64);
                let (input, mut inputs) = mpsc::channel::<TerminalInput>(64);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    loop {
                        match stdout.read(&mut buf).await {
                            Ok(0) | Err(_) => {
                                let _ = to_app.send(TerminalEvent::Exited { code: None }).await;
                                return;
                            }
                            Ok(n) => {
                                if to_app
                                    .send(TerminalEvent::Output(buf[..n].to_vec()))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                });
                tokio::spawn(async move {
                    while let Some(input) = inputs.recv().await {
                        let bytes = match input {
                            TerminalInput::Data(bytes) => bytes,
                            TerminalInput::Secret(secret) => secret.as_bytes().to_vec(),
                            _ => continue,
                        };
                        if stdin.write_all(&bytes).await.is_err() {
                            return;
                        }
                    }
                });
                Ok(TerminalStream {
                    input,
                    events,
                    terminator: Box::new(Kill(StdMutex::new(Some(child)))),
                })
            })
        }

        fn start_forward<'a>(
            &'a self,
            _forward: &'a tauminus_domain::Forward,
        ) -> BoxFuture<'a, Result<ForwardHandle, TunnelError>> {
            Box::pin(async { Err(TunnelError::Closed) })
        }

        fn open_sftp(&self) -> BoxFuture<'_, Result<Arc<dyn SftpPort>, ConnectError>> {
            Box::pin(async { Err(ConnectError::Cancelled) })
        }

        fn is_open(&self) -> bool {
            true
        }

        fn closed(&self) -> BoxFuture<'_, ()> {
            Box::pin(std::future::pending())
        }
    }

    struct LocalConnector(String);

    impl SshConnector for LocalConnector {
        fn connection<'a>(
            &'a self,
            _target: &'a SshTarget,
            _prompter: Arc<dyn Prompter>,
            _credentials: Arc<Credentials>,
            _forward_agent: bool,
        ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>> {
            let socket = self.0.clone();
            Box::pin(async move { Ok(Arc::new(Local { socket }) as Arc<dyn SshConnection>) })
        }

        fn connect_with<'a>(
            &'a self,
            _target: &'a SshTarget,
            _size: TerminalSize,
            _prompter: Arc<dyn Prompter>,
            _credentials: Arc<Credentials>,
        ) -> BoxFuture<'a, Result<TerminalStream, ConnectError>> {
            Box::pin(async { Err(ConnectError::Cancelled) })
        }

        fn run_script<'a>(
            &'a self,
            _target: &'a SshTarget,
            _prompter: Arc<dyn Prompter>,
            _credentials: Arc<Credentials>,
            _script: &'a str,
        ) -> BoxFuture<'a, Result<ScriptOutcome, ConnectError>> {
            Box::pin(async { Err(ConnectError::Cancelled) })
        }
    }

    /// Where this test's tmux keeps its socket: a folder of its own.
    fn dir(socket: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(socket)
    }

    /// Ends this test's tmux server, on its own socket only, and removes
    /// its folder.
    struct Server(String);

    impl Server {
        fn new(socket: String) -> Self {
            std::fs::create_dir_all(dir(&socket)).unwrap();
            Self(socket)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .args(["-L", &self.0, "kill-server"])
                .env_remove("TMUX")
                .env("TMUX_TMPDIR", dir(&self.0))
                .status();
            let _ = std::fs::remove_dir_all(dir(&self.0));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs tmux, on a socket of its own"]
    async fn real_tmux_survives_a_flood_the_view_falls_behind_on() {
        let socket = format!("tauminus-flood-{}", std::process::id());
        let _server = Server::new(socket.clone());
        let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
        let tmuxes = TmuxSessions::new(Arc::new(LocalConnector(socket)), Arc::clone(&sessions));
        let topology = Topology::default();
        let size = TerminalSize::new(100, 30).unwrap();
        let opened = tmuxes
            .open(&work_login(), size, prompter(), topology.clone())
            .await
            .unwrap();
        let flood = Sink::default();
        let id = tmuxes
            .attach_pane(opened.control, opened.windows[0].pane, size, flood.clone())
            .await
            .unwrap();
        // Never acknowledged while it floods: the pane falls behind.
        sessions
            .write(
                id,
                "yes tauminus | head -c 8000000; echo flood-done\r".into(),
            )
            .await
            .unwrap();
        let first = opened.windows[0].window;
        tmuxes.new_window(opened.control).unwrap();
        until("the second window", || {
            topology
                .events()
                .iter()
                .any(|e| matches!(e, TopologyEvent::Window(w) if w.window != first))
        })
        .await;
        let second = topology
            .events()
            .into_iter()
            .find_map(|e| match e {
                TopologyEvent::Window(w) if w.window != first => Some(w),
                _ => None,
            })
            .unwrap();
        let other = Sink::default();
        let two = tmuxes
            .attach_pane(opened.control, second.pane, size, other.clone())
            .await
            .unwrap();
        sessions
            .write(two, "echo still-$((40+2))\r".into())
            .await
            .unwrap();
        until("the other window answering", || {
            other.text().contains("still-42")
        })
        .await;
        // The view catches up, and the flooded pane comes back to its screen.
        for _ in 0..2000 {
            if flood.text().contains("\nflood-done") {
                break;
            }
            sessions.acknowledge(id, 1 << 20).unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            flood.text().contains("flood-done"),
            "the flooded pane resumed"
        );
        assert!(
            !topology
                .events()
                .iter()
                .any(|e| matches!(e, TopologyEvent::Ended { .. })),
            "tmux kept running"
        );
        tmuxes.detach(opened.control).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs tmux, on a socket of its own"]
    async fn real_tmux_shows_windows_as_sessions_and_types_into_them() {
        let socket = format!("tauminus-test-{}", std::process::id());
        let _server = Server::new(socket.clone());
        let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
        let tmuxes = TmuxSessions::new(Arc::new(LocalConnector(socket)), Arc::clone(&sessions));
        let topology = Topology::default();
        let opened = tmuxes
            .open(
                &work_login(),
                TerminalSize::new(100, 30).unwrap(),
                prompter(),
                topology.clone(),
            )
            .await
            .unwrap();
        assert_eq!(opened.windows.len(), 1);
        let first = opened.windows[0].clone();
        let sink = Sink::default();
        let id = tmuxes
            .attach_pane(
                opened.control,
                first.pane,
                TerminalSize::new(100, 30).unwrap(),
                sink.clone(),
            )
            .await
            .unwrap();
        sessions
            .write(id, "echo tauminus-$((6*7))\r".into())
            .await
            .unwrap();
        until("the echo", || sink.text().contains("tauminus-42")).await;
        tmuxes.new_window(opened.control).unwrap();
        until("the second window", || {
            topology
                .events()
                .iter()
                .any(|e| matches!(e, TopologyEvent::Window(w) if w.window != first.window))
        })
        .await;
        let second = topology
            .events()
            .into_iter()
            .find_map(|e| match e {
                TopologyEvent::Window(w) if w.window != first.window => Some(w),
                _ => None,
            })
            .unwrap();
        let other = Sink::default();
        let two = tmuxes
            .attach_pane(
                opened.control,
                second.pane,
                TerminalSize::new(100, 30).unwrap(),
                other.clone(),
            )
            .await
            .unwrap();
        sessions.write(two, "exit\r".into()).await.unwrap();
        until("the window's close", || {
            topology.events().contains(&TopologyEvent::Closed {
                window: second.window,
            })
        })
        .await;
        until("its session's end", || !lock(&other.exits).is_empty()).await;
        tmuxes.detach(opened.control).unwrap();
        until("the detach", || {
            topology
                .events()
                .iter()
                .any(|e| matches!(e, TopologyEvent::Ended { lost: false, .. }))
        })
        .await;
    }
}
