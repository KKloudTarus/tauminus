// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `AuthenticationMethods publickey,password`: the server lists only
//! public keys at first, and the password only after a key's partial
//! success. The identity's password must still answer, whether the vault
//! keeps it or a password manager does.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::decode_secret_key;
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use support::{Scratch, ScriptedPrompter, keygen};
use tauminus_app::ports::{Answer, BoxFuture, Credentials, Prompter, Secret, SshConnector};
use tauminus_app::vault::{SecretError, SecretRefs, SecretSource};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SecretRef, SecretSlot, SshTarget,
    TerminalSize, UserName,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};
use tokio::net::TcpListener;

const PASSWORD: &str = "hunter2";

/// After the key, the password: by the password method, or asked over
/// keyboard-interactive when `kbd`.
#[derive(Clone)]
struct KeyThenPassword {
    kbd: bool,
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait is async; this test server answers at once"
)]
impl server::Handler for KeyThenPassword {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        _key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let next = if self.kbd {
            MethodKind::KeyboardInteractive
        } else {
            MethodKind::Password
        };
        Ok(Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(&[next][..])),
            partial_success: true,
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(response) = response else {
            return Ok(Auth::Partial {
                name: "pam".into(),
                instructions: "".into(),
                prompts: vec![("Password: ".into(), false)].into(),
            });
        };
        Ok(if response.collect::<Vec<_>>() == [PASSWORD.as_bytes()] {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if password == PASSWORD {
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
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

async fn start(scratch: &Scratch, kbd: bool) -> u16 {
    keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", "host"]);
    keygen(
        &scratch.0,
        &["-q", "-t", "ed25519", "-N", "", "-f", "client"],
    );
    let host_key = decode_secret_key(
        &std::fs::read_to_string(scratch.path("host")).unwrap(),
        None,
    )
    .unwrap();
    let config = Arc::new(server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        ..server::Config::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                let handler = KeyThenPassword { kbd };
                if let Ok(session) = server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    port
}

fn target(scratch: &Scratch, port: u16, then: AuthMethod) -> SshTarget {
    let destination = Endpoint::new(
        UserName::parse("alice").unwrap(),
        HostName::parse("127.0.0.1").unwrap(),
        port,
    )
    .unwrap();
    let auth = AuthOptions {
        methods: vec![AuthMethod::PublicKey, then],
        identity_files: vec![scratch.path("client")],
        identities_only: true,
        identity_agent: None,
    };
    SshTarget::new(destination, vec![], Route::Direct, auth).unwrap()
}

struct Manager(Mutex<usize>);

impl SecretSource for Manager {
    fn fetch<'a>(
        &'a self,
        _reference: &'a SecretRef,
        _slot: SecretSlot,
    ) -> BoxFuture<'a, Result<Secret, SecretError>> {
        *self.0.lock().unwrap() += 1;
        Box::pin(async { Ok(Secret::new(PASSWORD.into())) })
    }
}

async fn sign_in(
    scratch: &Scratch,
    port: u16,
    then: AuthMethod,
    credentials: Credentials,
) -> Vec<String> {
    let store = HostKeyStore::new(vec![], scratch.path("known_hosts"));
    let connector = RusshConnector::new(store, None, AgentSource::None);
    // The host key, and nothing else: a password prompt is cancelled.
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Cancel,
    ]);
    connector
        .connect_with(
            &target(scratch, port, then),
            TerminalSize::new(80, 24).unwrap(),
            Arc::clone(&prompter) as Arc<dyn Prompter>,
            Arc::new(credentials),
        )
        .await
        .unwrap();
    prompter
        .asked()
        .iter()
        .map(|p| {
            format!("{p:?}")
                .split([' ', '{', '('])
                .next()
                .unwrap_or("")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn a_vault_password_answers_after_a_key_s_partial_success() {
    let scratch = Scratch::new("partial-vault");
    let port = start(&scratch, false).await;
    let asked = sign_in(
        &scratch,
        port,
        AuthMethod::Password,
        Credentials {
            password: Some(Secret::new(PASSWORD.into())),
            ..Credentials::default()
        },
    )
    .await;
    assert_eq!(asked, ["UnknownHostKey"]);
}

#[tokio::test]
async fn a_password_manager_s_password_answers_after_a_key_s_partial_success() {
    for (kbd, then) in [
        (false, AuthMethod::Password),
        (true, AuthMethod::KeyboardInteractive),
    ] {
        let scratch = Scratch::new(if kbd {
            "partial-kbd"
        } else {
            "partial-manager"
        });
        let port = start(&scratch, kbd).await;
        let manager = Arc::new(Manager(Mutex::new(0)));
        let asked = sign_in(
            &scratch,
            port,
            then,
            Credentials {
                references: SecretRefs {
                    password: Some(SecretRef::OnePassword {
                        reference: "op://Ops/db/password".into(),
                    }),
                    ..SecretRefs::default()
                },
                source: Some(Arc::clone(&manager) as Arc<dyn SecretSource>),
                ..Credentials::default()
            },
        )
        .await;
        assert_eq!(asked, ["UnknownHostKey"], "{then:?}");
        assert_eq!(*manager.0.lock().unwrap(), 1);
    }
}
