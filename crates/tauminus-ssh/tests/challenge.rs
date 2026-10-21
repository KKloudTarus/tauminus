// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Password and keyboard-interactive login against an in-process russh
//! server, which can script what OpenSSH will not: wrong answers, ten
//! challenge rounds, a server that never stops asking. `password_sshd.rs`
//! runs the plain cases against OpenSSH itself.

mod support;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use russh::keys::decode_secret_key;
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use support::{Scratch, ScriptedPrompter, keygen};
use tauminus_app::ports::{Answer, ConnectError, Prompt, Prompter, SshConnector};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, Route, SshTarget, TerminalSize, UserName,
};
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};
use tokio::net::TcpListener;

const PASSWORD: &str = "hunter2";
const CODE: &str = "123456";

/// Accepts password and two-factor logins. `rounds` sets how many
/// keyboard-interactive challenges come before the verdict; `None` never
/// stops asking.
#[derive(Clone)]
struct Server {
    rounds: Option<usize>,
    asked: usize,
}

impl Server {
    const fn new() -> Self {
        Self {
            rounds: Some(1),
            asked: 0,
        }
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait is async; this test server answers at once"
)]
impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if password == PASSWORD {
            Auth::Accept
        } else {
            reject()
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let challenge = |name: &'static str| Auth::Partial {
            name: Cow::Borrowed(name),
            instructions: Cow::Borrowed(""),
            prompts: Cow::Owned(vec![(Cow::Borrowed("Code: "), true)]),
        };
        if self.rounds != Some(1) {
            // Many rounds of one code each; accept after the last.
            if response.is_some() {
                self.asked += 1;
            }
            return Ok(match self.rounds {
                Some(rounds) if self.asked >= rounds => Auth::Accept,
                _ => challenge("again"),
            });
        }
        let Some(response) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed("two-factor"),
                instructions: Cow::Borrowed("Enter your password and the code"),
                prompts: Cow::Owned(vec![
                    (Cow::Borrowed("Password: "), false),
                    (Cow::Borrowed("Code: "), true),
                ]),
            });
        };
        let answers: Vec<_> = response.collect();
        Ok(if answers == [PASSWORD.as_bytes(), CODE.as_bytes()] {
            Auth::Accept
        } else {
            reject()
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
        session.data(channel, &b"hello from russh\r\n"[..])?;
        session.exit_status_request(channel, 7)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

fn reject() -> Auth {
    Auth::Reject {
        proceed_with_methods: Some(MethodSet::from(
            &[MethodKind::Password, MethodKind::KeyboardInteractive][..],
        )),
        partial_success: false,
    }
}

/// Starts the server on a free port and returns the port.
async fn start(scratch: &Scratch) -> u16 {
    start_with(scratch, Server::new()).await
}

async fn start_with(scratch: &Scratch, server: Server) -> u16 {
    keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", "host"]);
    let host_key = decode_secret_key(
        &std::fs::read_to_string(scratch.path("host")).unwrap(),
        None,
    )
    .unwrap();
    let config = Arc::new(server::Config {
        methods: MethodSet::from(&[MethodKind::Password, MethodKind::KeyboardInteractive][..]),
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

fn connector(scratch: &Scratch) -> RusshConnector {
    let store = HostKeyStore::new(vec![], scratch.path("known_hosts"));
    RusshConnector::new(store, None, AgentSource::None)
}

fn target(port: u16, methods: Vec<AuthMethod>) -> SshTarget {
    let destination = Endpoint::new(
        UserName::parse("alice").unwrap(),
        HostName::parse("127.0.0.1").unwrap(),
        port,
    )
    .unwrap();
    let auth = AuthOptions {
        methods,
        identity_files: vec![],
        identities_only: true,
    };
    SshTarget::new(destination, vec![], Route::Direct, auth).unwrap()
}

fn secret(text: &str) -> tauminus_app::ports::Secret {
    text.to_owned().into()
}

fn size() -> TerminalSize {
    TerminalSize::new(80, 24).unwrap()
}

fn dyn_prompter(p: &Arc<ScriptedPrompter>) -> Arc<dyn Prompter> {
    Arc::clone(p) as Arc<dyn Prompter>
}

#[tokio::test]
async fn password_is_asked_again_after_a_wrong_one() {
    let scratch = Scratch::new("password");
    let port = start(&scratch).await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(secret("wrong")),
        Answer::Secret(secret(PASSWORD)),
    ]);
    let mut stream = connector(&scratch)
        .connect(
            &target(port, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
        )
        .await
        .unwrap();
    let (output, code) = support::collect(&mut stream).await;
    assert!(output.contains("hello from russh"), "{output}");
    assert_eq!(code, Some(7));
    let passwords = prompter
        .asked()
        .iter()
        .filter(|p| matches!(p, Prompt::Password { endpoint } if endpoint.starts_with("alice@127.0.0.1")))
        .count();
    assert_eq!(passwords, 2);
}

#[tokio::test]
async fn three_wrong_passwords_fail() {
    let scratch = Scratch::new("password-fail");
    let port = start(&scratch).await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(secret("a")),
        Answer::Secret(secret("b")),
        Answer::Secret(secret("c")),
        Answer::Secret(secret(PASSWORD)),
    ]);
    let error = connector(&scratch)
        .connect(
            &target(port, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
        )
        .await
        .err()
        .unwrap();
    assert!(
        matches!(&error, ConnectError::AuthFailed { tried, .. } if tried == &["password"]),
        "{error:?}"
    );
}

#[tokio::test]
async fn cancelling_the_password_prompt_cancels_the_connect() {
    let scratch = Scratch::new("password-cancel");
    let port = start(&scratch).await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Cancel,
    ]);
    let error = connector(&scratch)
        .connect(
            &target(port, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(error, ConnectError::Cancelled), "{error:?}");
}

#[tokio::test]
async fn keyboard_interactive_shows_every_field_and_sends_the_answers() {
    let scratch = Scratch::new("kbd");
    let port = start(&scratch).await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Responses(vec![secret(PASSWORD), secret(CODE)]),
    ]);
    let methods = vec![AuthMethod::KeyboardInteractive, AuthMethod::Password];
    let mut stream = connector(&scratch)
        .connect(&target(port, methods), size(), dyn_prompter(&prompter))
        .await
        .unwrap();
    let (_, code) = support::collect(&mut stream).await;
    assert_eq!(code, Some(7));
    let asked = prompter.asked();
    let Some(Prompt::KeyboardInteractive {
        name,
        instructions,
        fields,
        ..
    }) = asked.get(1)
    else {
        panic!("expected a keyboard-interactive prompt, got {asked:?}");
    };
    assert_eq!(name, "two-factor");
    assert_eq!(instructions, "Enter your password and the code");
    let shown: Vec<_> = fields.iter().map(|f| (f.text.as_str(), f.echo)).collect();
    assert_eq!(shown, [("Password: ", false), ("Code: ", true)]);
    assert_eq!(
        asked.len(),
        2,
        "password was not needed after keyboard-interactive"
    );
}

#[tokio::test]
async fn wrong_challenge_answers_fall_through_to_password() {
    let scratch = Scratch::new("kbd-fallback");
    let port = start(&scratch).await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Responses(vec![secret(PASSWORD), secret("000000")]),
        Answer::Secret(secret(PASSWORD)),
    ]);
    let methods = vec![AuthMethod::KeyboardInteractive, AuthMethod::Password];
    let mut stream = connector(&scratch)
        .connect(&target(port, methods), size(), dyn_prompter(&prompter))
        .await
        .unwrap();
    let (_, code) = support::collect(&mut stream).await;
    assert_eq!(code, Some(7));
}

/// Answers every challenge with a code, counting the rounds.
struct Codes(std::sync::atomic::AtomicUsize);

impl Prompter for Codes {
    fn ask(&self, prompt: Prompt) -> tauminus_app::ports::BoxFuture<'_, Answer> {
        let answer = match prompt {
            Prompt::UnknownHostKey { .. } => Answer::TrustHostKey { remember: false },
            Prompt::KeyboardInteractive { .. } => {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Answer::Responses(vec![secret(CODE)])
            }
            _ => Answer::Cancel,
        };
        Box::pin(async move { answer })
    }
}

#[tokio::test]
async fn the_tenth_challenge_round_still_logs_in() {
    let scratch = Scratch::new("kbd-ten");
    let port = start_with(
        &scratch,
        Server {
            rounds: Some(10),
            asked: 0,
        },
    )
    .await;
    let codes = Arc::new(Codes(std::sync::atomic::AtomicUsize::new(0)));
    let mut stream = connector(&scratch)
        .connect(
            &target(port, vec![AuthMethod::KeyboardInteractive]),
            size(),
            Arc::clone(&codes) as Arc<dyn Prompter>,
        )
        .await
        .unwrap();
    let (_, code) = support::collect(&mut stream).await;
    assert_eq!(code, Some(7));
    assert_eq!(codes.0.load(std::sync::atomic::Ordering::Relaxed), 10);
}

#[tokio::test]
async fn a_server_that_never_stops_asking_is_given_up_on() {
    let scratch = Scratch::new("kbd-forever");
    let port = start_with(
        &scratch,
        Server {
            rounds: None,
            asked: 0,
        },
    )
    .await;
    let codes = Arc::new(Codes(std::sync::atomic::AtomicUsize::new(0)));
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        connector(&scratch).connect(
            &target(port, vec![AuthMethod::KeyboardInteractive]),
            size(),
            Arc::clone(&codes) as Arc<dyn Prompter>,
        ),
    )
    .await
    .expect("the client stops on its own");
    assert!(
        matches!(result, Err(ConnectError::AuthFailed { .. })),
        "{:?}",
        result.err()
    );
    assert_eq!(codes.0.load(std::sync::atomic::Ordering::Relaxed), 10);
}
