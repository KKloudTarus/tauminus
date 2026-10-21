// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Agent login on Windows, through the ssh-agent service of OpenSSH for
//! Windows and through PuTTY's Pageant. CI starts both and sets
//! `TAUMINUS_WINDOWS_AGENTS`; the tests skip without it, except in CI on
//! Windows. Each test puts its own key in the agent through russh's own
//! agent client, then logs in to an in-process russh server that accepts
//! only that key, so the login can only have used the agent.

#![cfg(windows)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use russh::keys::agent::client::AgentClient;
use russh::keys::{PrivateKey, PublicKey, decode_secret_key};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use support::{Scratch, ScriptedPrompter, collect, keygen};
use tauminus_app::ports::{Prompter, SshConnector};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, TerminalSize, UserName,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};
use tokio::net::TcpListener;

/// Whether CI started the agents.
fn enabled() -> bool {
    if std::env::var_os("TAUMINUS_WINDOWS_AGENTS").is_some() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI on Windows must start ssh-agent and Pageant"
    );
    false
}

#[derive(Clone)]
struct Server {
    accepted: PublicKey,
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait is async; this test server answers at once"
)]
impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(if key.key_data() == self.accepted.key_data() {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, &b"signed by the agent\r\n"[..])?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

/// A fresh Ed25519 key made by ssh-keygen in the scratch directory.
fn user_key(scratch: &Scratch, name: &str) -> PrivateKey {
    keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", name]);
    decode_secret_key(&std::fs::read_to_string(scratch.path(name)).unwrap(), None).unwrap()
}

/// Starts a server that accepts only `accepted` and returns its port.
async fn start(scratch: &Scratch, accepted: PublicKey) -> u16 {
    let host_key = user_key(scratch, "host");
    let config = Arc::new(server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        ..server::Config::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = Server { accepted };
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = Arc::clone(&config);
            let handler = server.clone();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    port
}

async fn log_in(scratch: &Scratch, port: u16, agent: AgentSource) -> Option<u32> {
    let store = HostKeyStore::new(vec![], scratch.path("known_hosts"));
    let connector = RusshConnector::new(store, None, agent);
    let destination = Endpoint::new(
        UserName::parse("agent-user").unwrap(),
        HostName::parse("127.0.0.1").unwrap(),
        port,
    )
    .unwrap();
    let auth = AuthOptions {
        methods: vec![AuthMethod::Agent],
        identity_files: vec![],
        identities_only: false,
    };
    let target = SshTarget::new(destination, vec![], Route::Direct, auth).unwrap();
    let prompter = ScriptedPrompter::trust();
    let mut stream = connector
        .connect(
            &target,
            TerminalSize::new(80, 24).unwrap(),
            Arc::clone(&prompter) as Arc<dyn Prompter>,
        )
        .await
        .unwrap();
    let (output, code) = collect(&mut stream).await;
    assert!(output.contains("signed by the agent"), "{output}");
    code
}

#[tokio::test]
async fn the_openssh_agent_service_signs_the_login() {
    if !enabled() {
        return;
    }
    let scratch = Scratch::new("win-openssh-agent");
    let key = user_key(&scratch, "id");
    let mut agent = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
        .await
        .expect("the ssh-agent service is running");
    agent.add_identity(&key, &[]).await.unwrap();
    let port = start(&scratch, key.public_key().clone()).await;
    // SSH_AUTH_SOCK is unset in CI: the default pipe comes first.
    assert_eq!(
        log_in(&scratch, port, AgentSource::Environment).await,
        Some(0)
    );
    agent.remove_identity(key.public_key()).await.unwrap();
}

#[tokio::test]
async fn pageant_signs_the_login() {
    if !enabled() {
        return;
    }
    let scratch = Scratch::new("win-pageant");
    let key = user_key(&scratch, "id");
    let mut agent = AgentClient::connect_pageant()
        .await
        .expect("Pageant is running");
    agent.add_identity(&key, &[]).await.unwrap();
    let port = start(&scratch, key.public_key().clone()).await;
    assert_eq!(log_in(&scratch, port, AgentSource::Pageant).await, Some(0));
    agent.remove_identity(key.public_key()).await.unwrap();
}
