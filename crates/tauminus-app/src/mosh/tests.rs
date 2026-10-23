// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex;

use tauminus_domain::{
    AuthOptions, Endpoint, HostId, HostName, MoshConnect, Route, SshTarget, UserName,
};
use tokio::sync::mpsc;

use super::*;
use crate::SessionConfig;
use crate::ports::{
    BoxFuture, Credentials, ScriptOutcome, SinkClosed, SshConnection, TerminalStream, Terminator,
};

/// Answers every script with `output`, and remembers the scripts.
struct ScriptedServer {
    output: String,
    scripts: Mutex<Vec<String>>,
}

impl SshConnector for ScriptedServer {
    fn connection<'a>(
        &'a self,
        _target: &'a SshTarget,
        _prompter: Arc<dyn Prompter>,
        _credentials: Arc<Credentials>,
        _forward_agent: bool,
    ) -> BoxFuture<'a, Result<Arc<dyn SshConnection>, ConnectError>> {
        Box::pin(async { Err(ConnectError::Cancelled) })
    }

    fn run_script<'a>(
        &'a self,
        _target: &'a SshTarget,
        _prompter: Arc<dyn Prompter>,
        _credentials: Arc<Credentials>,
        script: &'a str,
    ) -> BoxFuture<'a, Result<ScriptOutcome, ConnectError>> {
        self.scripts.lock().unwrap().push(script.to_owned());
        let output = self.output.clone();
        Box::pin(async move {
            Ok(ScriptOutcome {
                status: Some(0),
                output,
                peer: Some(std::net::Ipv4Addr::new(192, 0, 2, 7).into()),
            })
        })
    }
}

#[derive(Default)]
struct RecordingClient {
    launched: Mutex<Vec<(String, u16, String)>>,
    /// No `mosh-client` on this computer.
    missing: bool,
}

struct Nothing;

impl Terminator for Nothing {
    fn terminate(&self) {}
}

impl MoshClient for RecordingClient {
    fn ready(&self) -> Result<(), LaunchError> {
        if self.missing {
            return Err(LaunchError::Spawn {
                program: "mosh-client".into(),
                source: std::io::ErrorKind::NotFound.into(),
            });
        }
        Ok(())
    }

    fn launch<'a>(
        &'a self,
        address: std::net::IpAddr,
        connect: &'a MoshConnect,
        _size: TerminalSize,
    ) -> BoxFuture<'a, Result<TerminalStream, LaunchError>> {
        self.launched.lock().unwrap().push((
            address.to_string(),
            connect.port,
            connect.key.as_str().to_owned(),
        ));
        let (input, _) = mpsc::channel(1);
        let (_, events) = mpsc::channel(1);
        Box::pin(async move {
            Ok(TerminalStream {
                input,
                events,
                terminator: Box::new(Nothing),
            })
        })
    }
}

struct Sink;

impl OutputSink for Sink {
    fn send_output(&self, _batch: Vec<u8>) -> Result<(), SinkClosed> {
        Ok(())
    }

    fn send_exit(&self, _code: Option<u32>) {}
}

struct NoPrompts;

impl Prompter for NoPrompts {
    fn ask(&self, _prompt: crate::ports::Prompt) -> BoxFuture<'_, crate::ports::Answer> {
        Box::pin(async { crate::ports::Answer::Cancel })
    }
}

fn login(route: Route) -> Login {
    let destination = Endpoint::new(
        UserName::parse("ops").unwrap(),
        HostName::parse("db.example").unwrap(),
        22,
    )
    .unwrap();
    Login {
        host: HostId::parse("db").unwrap(),
        target: SshTarget::new(destination, vec![], route, AuthOptions::default()).unwrap(),
        identity: None,
        credentials: Arc::default(),
    }
}

fn sessions(output: &str) -> (MoshSessions, Arc<ScriptedServer>, Arc<RecordingClient>) {
    let server = Arc::new(ScriptedServer {
        output: output.into(),
        scripts: Mutex::default(),
    });
    let client = Arc::new(RecordingClient::default());
    let manager = Arc::new(SessionManager::new(SessionConfig::default()));
    (
        MoshSessions::new(
            Arc::clone(&server) as Arc<dyn SshConnector>,
            Arc::clone(&client) as Arc<dyn MoshClient>,
            manager,
        ),
        server,
        client,
    )
}

#[tokio::test]
async fn the_server_is_started_over_ssh_and_the_client_aimed_at_it() {
    let (mosh, server, client) =
        sessions("MOSH CONNECT 60004 4tWIc8U4T0theuVzWIX0Pg\n[mosh-server detached, pid = 7]\n");
    let id = mosh
        .open_saved(
            &login(Route::Direct),
            TerminalSize::default(),
            Sink,
            Arc::new(NoPrompts),
        )
        .await
        .unwrap();
    // The session knows its saved host: what the vault may type into it
    // depends on that.
    let saved = mosh.sessions.meta(id).unwrap().host.unwrap();
    assert_eq!(saved.id, HostId::parse("db").unwrap());
    assert_eq!(saved.login.host.as_str(), "db.example");
    assert_eq!(*server.scripts.lock().unwrap(), [MOSH_SERVER_SCRIPT]);
    assert_eq!(
        *client.launched.lock().unwrap(),
        [(
            // Where SSH reached it, not a second lookup of the name.
            "192.0.2.7".to_owned(),
            60004,
            "4tWIc8U4T0theuVzWIX0Pg".to_owned()
        )]
    );
}

#[tokio::test]
async fn a_host_behind_a_proxy_or_without_mosh_server_is_refused() {
    let (mosh, server, _) = sessions("");
    let routed = mosh
        .open_saved(
            &login(Route::ProxyCommand("nc %h %p".into())),
            TerminalSize::default(),
            Sink,
            Arc::new(NoPrompts),
        )
        .await;
    assert!(matches!(routed, Err(MoshOpenError::Routed)));
    assert!(server.scripts.lock().unwrap().is_empty());

    let (mosh, _, client) = sessions("sh: 4: mosh-server: not found\n");
    let missing = mosh
        .open_saved(
            &login(Route::Direct),
            TerminalSize::default(),
            Sink,
            Arc::new(NoPrompts),
        )
        .await;
    assert!(matches!(
        missing,
        Err(MoshOpenError::Server(MoshError::NoServer))
    ));
    assert!(client.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn no_client_here_means_no_sign_in_there() {
    let server = Arc::new(ScriptedServer {
        output: String::new(),
        scripts: Mutex::default(),
    });
    let client = Arc::new(RecordingClient {
        missing: true,
        ..RecordingClient::default()
    });
    let mosh = MoshSessions::new(
        Arc::clone(&server) as Arc<dyn SshConnector>,
        client as Arc<dyn MoshClient>,
        Arc::new(SessionManager::new(SessionConfig::default())),
    );
    let opened = mosh
        .open_saved(
            &login(Route::Direct),
            TerminalSize::default(),
            Sink,
            Arc::new(NoPrompts),
        )
        .await;
    assert!(matches!(opened, Err(MoshOpenError::Client(_))));
    assert!(server.scripts.lock().unwrap().is_empty());
}
