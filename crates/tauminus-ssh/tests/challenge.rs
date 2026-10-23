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
/// A stored password the server no longer takes; no other test sends it.
const ROTATED: &str = "rotated-away";
static ROTATED_TRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The same, for the test where the server closes right after turning it down.
const ROTATED_LAST: &str = "rotated-and-last";
/// The same, for panes signing in at the same moment.
const ROTATED_TOGETHER: &str = "rotated-together";
static ROTATED_TOGETHER_TRIES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static ROTATED_LAST_TRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Accepts password and two-factor logins. `rounds` sets how many
/// keyboard-interactive challenges come before the verdict; `None` never
/// stops asking.
#[derive(Clone)]
struct Server {
    rounds: Option<usize>,
    asked: usize,
    /// Asks one hidden field, with this prompt, and accepts this answer.
    single: Option<(&'static str, &'static str)>,
    /// Drops the connection when a password arrives, as Wi-Fi going away
    /// mid-login does.
    cut: Option<Arc<tokio::sync::Notify>>,
    /// Sign-in requests refused before the server says goodbye.
    max_tries: usize,
    /// Keyboard-interactive in two rounds, as a PAM stack asks: the
    /// password, then a code.
    two_step: bool,
}

impl Server {
    const fn new() -> Self {
        Self {
            rounds: Some(1),
            asked: 0,
            single: None,
            cut: None,
            max_tries: 10,
            two_step: false,
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
        if let Some(cut) = &self.cut {
            cut.notify_one();
            std::future::pending::<()>().await;
        }
        if password == ROTATED {
            ROTATED_TRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if password == ROTATED_TOGETHER {
            ROTATED_TOGETHER_TRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if password == ROTATED_LAST {
            ROTATED_LAST_TRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
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
        if self.two_step {
            let prompt = |text: &'static str| Auth::Partial {
                name: Cow::Borrowed("pam"),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed(text), false)]),
            };
            let Some(response) = response else {
                self.asked = 0;
                return Ok(prompt("Password: "));
            };
            let answer: Vec<_> = response.collect();
            self.asked += 1;
            return Ok(match self.asked {
                1 if answer == [PASSWORD.as_bytes()] => prompt("Verification code: "),
                2 if answer == [CODE.as_bytes()] => Auth::Accept,
                _ => reject(),
            });
        }
        if let Some((prompt, answer)) = self.single {
            return Ok(match response {
                None => Auth::Partial {
                    name: Cow::Borrowed("single"),
                    instructions: Cow::Borrowed(""),
                    prompts: Cow::Owned(vec![(Cow::Borrowed(prompt), false)]),
                },
                Some(response) => {
                    if response.collect::<Vec<_>>() == [answer.as_bytes()] {
                        Auth::Accept
                    } else {
                        reject()
                    }
                }
            });
        }
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
        // Keys are offered and all refused, by russh's default handler.
        methods: MethodSet::from(
            &[
                MethodKind::PublicKey,
                MethodKind::Password,
                MethodKind::KeyboardInteractive,
            ][..],
        ),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        max_auth_attempts: server.max_tries,
        ..server::Config::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let config = Arc::clone(&config);
            let handler = server.clone();
            tokio::spawn(async move {
                // A cut shuts the socket under the session: the client
                // sees the link end, as on a network drop.
                if let Some(cut) = handler.cut.clone() {
                    let raw = socket.into_std().unwrap();
                    let twin = raw.try_clone().unwrap();
                    let socket = tokio::net::TcpStream::from_std(raw).unwrap();
                    tokio::spawn(async move {
                        cut.notified().await;
                        let _ = twin.shutdown(std::net::Shutdown::Both);
                    });
                    if let Ok(session) = server::run_stream(config, socket, handler).await {
                        let _ = session.await;
                    }
                } else if let Ok(session) = server::run_stream(config, socket, handler).await {
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
            ..Server::new()
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
            ..Server::new()
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

#[tokio::test]
async fn a_stored_password_never_answers_a_one_time_code() {
    let scratch = Scratch::new("kbd-otp");
    let port = start_with(
        &scratch,
        Server {
            single: Some(("Verification code: ", CODE)),
            ..Server::new()
        },
    )
    .await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Responses(vec![secret(CODE)]),
    ]);
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(PASSWORD)),
        ..tauminus_app::ports::Credentials::default()
    });
    connector(&scratch)
        .connect_with(
            &target(port, vec![AuthMethod::KeyboardInteractive]),
            size(),
            dyn_prompter(&prompter),
            credentials,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            prompter.asked().get(1),
            Some(Prompt::KeyboardInteractive { .. })
        ),
        "the code was asked of the user: {:?}",
        prompter.asked()
    );
}

#[tokio::test]
async fn a_wrong_stored_password_over_keyboard_interactive_then_asks() {
    let scratch = Scratch::new("kbd-stale");
    let port = start_with(
        &scratch,
        Server {
            single: Some(("Password: ", PASSWORD)),
            ..Server::new()
        },
    )
    .await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Responses(vec![secret(PASSWORD)]),
    ]);
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret("stale")),
        ..tauminus_app::ports::Credentials::default()
    });
    connector(&scratch)
        .connect_with(
            &target(port, vec![AuthMethod::KeyboardInteractive]),
            size(),
            dyn_prompter(&prompter),
            credentials,
        )
        .await
        .unwrap();
    assert_eq!(prompter.asked().len(), 2, "{:?}", prompter.asked());
}

/// A password changed on the server: the first connect spends the old one
/// once, and later connects (a wake reconnecting every pane) ask instead of
/// spending one of the account's tries each.
#[tokio::test]
async fn a_stored_password_turned_down_once_is_not_sent_again() {
    let scratch = Scratch::new("pw-rotated");
    let port = start_with(&scratch, Server::new()).await;
    let connector = connector(&scratch);
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(ROTATED)),
        ..tauminus_app::ports::Credentials::default()
    });
    for round in 0..3 {
        let mut answers = vec![Answer::Secret(secret(PASSWORD))];
        if round == 0 {
            answers.insert(0, Answer::TrustHostKey { remember: true });
        }
        let prompter = ScriptedPrompter::new(answers);
        connector
            .connect_with(
                &target(port, vec![AuthMethod::Password]),
                size(),
                dyn_prompter(&prompter),
                Arc::clone(&credentials),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        ROTATED_TRIES.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the old password went out once"
    );
}

/// The link goes away in the middle of signing in: a reconnect tries again
/// later instead of taking it for a refusal.
#[tokio::test]
async fn a_link_lost_while_signing_in_is_unreachable_not_refused() {
    let scratch = Scratch::new("pw-dropped");
    let dropping = start_with(
        &scratch,
        Server {
            cut: Some(Arc::default()),
            ..Server::new()
        },
    )
    .await;
    let connector = connector(&scratch);
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(PASSWORD)),
        ..tauminus_app::ports::Credentials::default()
    });
    let prompter = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: false }]);
    let Err(error) = connector
        .connect_with(
            &target(dropping, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
            Arc::clone(&credentials),
        )
        .await
    else {
        panic!("the server dropped the link");
    };
    assert!(
        matches!(error, ConnectError::Io(_)),
        "{error:?} {:?}",
        prompter.asked()
    );
    // The password was not turned down: the next connect still sends it.
    let scratch = Scratch::new("pw-after-drop");
    let fine = start(&scratch).await;
    let prompter = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: false }]);
    connector
        .connect_with(
            &target(fine, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
            credentials,
        )
        .await
        .unwrap();
    assert_eq!(prompter.asked().len(), 1, "{:?}", prompter.asked());
}

/// A server that says goodbye after too many tries, as OpenSSH does past
/// `MaxAuthTries`, refused: reconnecting would only spend more tries.
#[tokio::test]
async fn a_server_that_closes_after_too_many_tries_refused() {
    let scratch = Scratch::new("pw-goodbye");
    let port = start_with(
        &scratch,
        Server {
            max_tries: 1,
            ..Server::new()
        },
    )
    .await;
    let prompter = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: false },
        Answer::Secret(secret(PASSWORD)),
    ]);
    let Err(error) = connector(&scratch)
        .connect_with(
            &target(port, vec![AuthMethod::Password]),
            size(),
            dyn_prompter(&prompter),
            Arc::default(),
        )
        .await
    else {
        panic!("the server gave up on the login");
    };
    let ConnectError::Protocol(message) = &error else {
        panic!("{error:?}");
    };
    assert!(
        message.contains("Too many authentication attempts"),
        "{message}"
    );
}

/// The goodbye comes while keys are being offered: the sign-in that
/// notices it on its next key still takes it for a refusal.
#[tokio::test]
async fn a_goodbye_between_keys_is_a_refusal() {
    let scratch = Scratch::new("keys-goodbye");
    let port = start_with(
        &scratch,
        Server {
            max_tries: 2,
            ..Server::new()
        },
    )
    .await;
    let mut files = Vec::new();
    for name in ["k1", "k2", "k3", "k4"] {
        keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", name]);
        files.push(scratch.path(name));
    }
    let mut target = target(port, vec![AuthMethod::PublicKey]);
    target.auth.identity_files = files;
    let prompter = ScriptedPrompter::new(vec![Answer::TrustHostKey { remember: false }]);
    let Err(error) = connector(&scratch)
        .connect_with(&target, size(), dyn_prompter(&prompter), Arc::default())
        .await
    else {
        panic!("every key was refused");
    };
    let ConnectError::Protocol(message) = &error else {
        panic!("{error:?}");
    };
    assert!(
        message.contains("Too many authentication attempts"),
        "{message}"
    );
}

/// The stale password is the last try the server takes before it closes
/// the connection: it is still not sent again.
#[tokio::test]
async fn a_password_turned_down_just_before_a_goodbye_is_not_sent_again() {
    let scratch = Scratch::new("pw-last");
    let port = start_with(
        &scratch,
        Server {
            max_tries: 2,
            ..Server::new()
        },
    )
    .await;
    let connector = connector(&scratch);
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(ROTATED_LAST)),
        ..tauminus_app::ports::Credentials::default()
    });
    for round in 0..2 {
        let mut answers = vec![Answer::Secret(secret("also-wrong"))];
        if round == 0 {
            answers.insert(0, Answer::TrustHostKey { remember: true });
        }
        let prompter = ScriptedPrompter::new(answers);
        let _ = connector
            .connect_with(
                &target(port, vec![AuthMethod::Password]),
                size(),
                dyn_prompter(&prompter),
                Arc::clone(&credentials),
            )
            .await;
    }
    assert_eq!(
        ROTATED_LAST_TRIES.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

/// Six panes sign in at the same moment after a wake, with a password that
/// changed on the server: it goes out once, and the others ask instead.
#[tokio::test]
async fn panes_signing_in_together_spend_a_stale_password_once() {
    let scratch = Scratch::new("pw-together");
    let port = start(&scratch).await;
    let connector = Arc::new(connector(&scratch));
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(ROTATED_TOGETHER)),
        ..tauminus_app::ports::Credentials::default()
    });
    let mut panes = Vec::new();
    for _ in 0..6 {
        let connector = Arc::clone(&connector);
        let credentials = Arc::clone(&credentials);
        panes.push(tokio::spawn(async move {
            let prompter = ScriptedPrompter::new(vec![
                Answer::TrustHostKey { remember: false },
                Answer::Secret(secret(PASSWORD)),
            ]);
            connector
                .connect_with(
                    &target(port, vec![AuthMethod::Password]),
                    size(),
                    dyn_prompter(&prompter),
                    credentials,
                )
                .await
                .map(drop)
        }));
    }
    for pane in panes {
        assert!(matches!(pane.await, Ok(Ok(()))));
    }
    assert_eq!(
        ROTATED_TOGETHER_TRIES.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

/// The stored password was right and the code was wrong: the password is
/// still sent next time.
#[tokio::test]
async fn a_wrong_code_after_the_stored_password_does_not_mark_it() {
    let scratch = Scratch::new("kbd-two-step");
    let port = start_with(
        &scratch,
        Server {
            two_step: true,
            ..Server::new()
        },
    )
    .await;
    let connector = connector(&scratch);
    let credentials = Arc::new(tauminus_app::ports::Credentials {
        password: Some(secret(PASSWORD)),
        ..tauminus_app::ports::Credentials::default()
    });
    let wrong = ScriptedPrompter::new(vec![
        Answer::TrustHostKey { remember: true },
        Answer::Responses(vec![secret("000000")]),
        Answer::Cancel,
    ]);
    let _ = connector
        .connect_with(
            &target(port, vec![AuthMethod::KeyboardInteractive]),
            size(),
            dyn_prompter(&wrong),
            Arc::clone(&credentials),
        )
        .await;
    let right = ScriptedPrompter::new(vec![Answer::Responses(vec![secret(CODE)])]);
    connector
        .connect_with(
            &target(port, vec![AuthMethod::KeyboardInteractive]),
            size(),
            dyn_prompter(&right),
            credentials,
        )
        .await
        .unwrap();
}
