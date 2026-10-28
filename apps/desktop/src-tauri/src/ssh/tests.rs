// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::mpsc;

use tauri::ipc::InvokeResponseBody;

use super::*;

fn endpoint(user: &str, host: &str, port: u16) -> EndpointSpec {
    EndpointSpec {
        user: user.into(),
        host: host.into(),
        port,
    }
}

fn spec(route: RouteSpec) -> SshTargetSpec {
    SshTargetSpec {
        destination: endpoint("alice", "db.example.com", 22),
        jumps: vec![],
        route,
        methods: None,
        identity_files: vec![],
        identities_only: false,
        identity_agent: None,
    }
}

#[test]
fn the_identity_agent_reads_as_ssh_config_writes_it() {
    for (text, expected) in [
        ("none", Some(IdentityAgent::None)),
        ("SSH_AUTH_SOCK", Some(IdentityAgent::Environment)),
        (
            "~/secretive/socket.ssh",
            Some(IdentityAgent::Path("~/secretive/socket.ssh".into())),
        ),
    ] {
        let target = SshTarget::try_from(SshTargetSpec {
            identity_agent: Some(text.into()),
            ..spec(RouteSpec::Direct)
        })
        .unwrap();
        assert_eq!(target.auth.identity_agent, expected, "{text}");
        // And back, as the view gets it.
        assert_eq!(
            SshTargetSpec::from(&target).identity_agent.as_deref(),
            Some(text)
        );
    }
    for bad in ["", "a\nb"] {
        assert!(
            SshTarget::try_from(SshTargetSpec {
                identity_agent: Some(bad.into()),
                ..spec(RouteSpec::Direct)
            })
            .is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn target_spec_converts_with_default_auth() {
    let mut input = spec(RouteSpec::Direct);
    input.jumps = vec![endpoint("ops", "bastion", 2222)];
    let target = SshTarget::try_from(input).unwrap();
    assert_eq!(target.destination.to_string(), "alice@db.example.com");
    assert_eq!(target.jumps[0].to_string(), "ops@bastion:2222");
    assert_eq!(target.auth.methods, AuthOptions::default().methods);
}

#[test]
fn empty_user_means_the_local_user() {
    let mut input = spec(RouteSpec::Direct);
    input.destination.user = String::new();
    let target = SshTarget::try_from(input).unwrap();
    assert_eq!(target.destination.user.as_str(), local_user().unwrap());
}

#[test]
fn target_spec_rejects_bad_names_and_shapes() {
    let cases = [
        spec(RouteSpec::Direct).with_destination(endpoint("alice", "db;rm -rf ~", 22)),
        spec(RouteSpec::Direct).with_destination(endpoint("-oProxyCommand=x", "db", 22)),
        spec(RouteSpec::Direct).with_destination(endpoint("alice", "db", 0)),
        spec(RouteSpec::ProxyCommand {
            command: "  ".into(),
        }),
        spec(RouteSpec::ProxyCommand {
            command: "x".repeat(MAX_PROXY_COMMAND + 1),
        }),
        spec(RouteSpec::Socks5 {
            host: "proxy host".into(),
            port: 1080,
        }),
        {
            let mut s = spec(RouteSpec::ProxyCommand {
                command: "nc %h %p".into(),
            });
            s.jumps = vec![endpoint("a", "b", 22)];
            s
        },
        {
            let mut s = spec(RouteSpec::Direct);
            s.methods = Some(vec![]);
            s
        },
        {
            let mut s = spec(RouteSpec::Direct);
            s.jumps = (0..=SshTarget::MAX_JUMPS)
                .map(|i| endpoint("a", &format!("h{i}"), 22))
                .collect();
            s
        },
        {
            let mut s = spec(RouteSpec::Direct);
            s.identity_files = vec!["k".into(); MAX_IDENTITY_FILES + 1];
            s
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        let error = SshTarget::try_from(case)
            .err()
            .unwrap_or_else(|| panic!("case {index} accepted"));
        assert_eq!(error.code, ErrorCode::InvalidArgument, "case {index}");
    }
}

impl SshTargetSpec {
    fn with_destination(mut self, destination: EndpointSpec) -> Self {
        self.destination = destination;
        self
    }
}

#[test]
fn route_and_auth_spec_parse_from_json() {
    let json = r#"{
        "destination": {"user": "alice", "host": "db", "port": 22},
        "route": {"kind": "httpConnect", "host": "proxy", "port": 3128},
        "methods": ["publicKey", "password"],
        "identityFiles": ["~/.ssh/id_work"],
        "identitiesOnly": true
    }"#;
    let target = SshTarget::try_from(serde_json::from_str::<SshTargetSpec>(json).unwrap()).unwrap();
    assert!(matches!(
        target.route,
        Route::HttpConnect { port: 3128, .. }
    ));
    assert_eq!(
        target.auth.methods,
        [AuthMethod::PublicKey, AuthMethod::Password]
    );
    assert!(target.auth.identities_only);
}

#[test]
fn answers_parse_and_never_print_secrets() {
    let answer: AnswerSpec =
        serde_json::from_str(r#"{"kind": "secret", "value": "hunter2"}"#).unwrap();
    assert_eq!(format!("{answer:?}"), "Secret");
    assert!(matches!(Answer::from(answer), Answer::Secret(s) if s.as_str() == "hunter2"));
    let answer: AnswerSpec =
        serde_json::from_str(r#"{"kind": "trustHostKey", "remember": true}"#).unwrap();
    assert!(matches!(
        Answer::from(answer),
        Answer::TrustHostKey { remember: true }
    ));
}

#[test]
fn prompts_serialize_with_a_kind_tag() {
    let request = PromptRequest {
        id: 3,
        prompt: Prompt::UnknownHostKey {
            host: "db".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            certificate_expected: false,
        }
        .into(),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["id"], 3);
    assert_eq!(json["prompt"]["kind"], "unknownHostKey");
    assert_eq!(json["prompt"]["certificateExpected"], false);
}

#[test]
fn broker_delivers_once_and_forgets() {
    let broker = PromptBroker::default();
    let (id, mut rx) = broker.register();
    assert!(broker.reply(id, Answer::TrustHostKey { remember: false }));
    assert!(matches!(
        rx.try_recv(),
        Ok(Answer::TrustHostKey { remember: false })
    ));
    assert!(
        !broker.reply(id, Answer::Cancel),
        "a second reply finds nothing"
    );
    assert!(!broker.reply(999, Answer::Cancel), "unknown id");
    assert_eq!(broker.lock().pending.len(), 0);
}

#[test]
fn cancel_all_answers_every_waiting_prompt() {
    let broker = PromptBroker::default();
    let (_, mut a) = broker.register();
    let (_, mut b) = broker.register();
    assert_eq!(broker.cancel_all(), 2);
    assert!(matches!(a.try_recv(), Ok(Answer::Cancel)));
    assert!(matches!(b.try_recv(), Ok(Answer::Cancel)));
    assert_eq!(broker.lock().pending.len(), 0);
}

/// A prompt channel whose messages land in a std channel.
fn capture() -> (Channel<PromptRequest>, mpsc::Receiver<serde_json::Value>) {
    let (tx, rx) = mpsc::channel();
    let channel = Channel::new(move |body| {
        if let InvokeResponseBody::Json(json) = body {
            let _ = tx.send(serde_json::from_str(&json).unwrap());
        }
        Ok(())
    });
    (channel, rx)
}

#[tokio::test]
async fn prompter_sends_the_request_and_returns_the_reply() {
    let broker = Arc::new(PromptBroker::default());
    let (channel, sent) = capture();
    let prompter = ChannelPrompter {
        broker: Arc::clone(&broker),
        channel,
        timeout: PROMPT_TIMEOUT,
    };
    let ask = prompter.ask(Prompt::Password {
        endpoint: "alice@db:22".into(),
    });
    let replier = {
        let broker = Arc::clone(&broker);
        tokio::task::spawn_blocking(move || {
            let request = sent.recv().unwrap();
            assert_eq!(request["prompt"]["kind"], "password");
            let id = PromptId::try_from(request["id"].as_u64().unwrap()).unwrap();
            assert!(broker.reply(id, Answer::Secret(Zeroizing::new("pw".into()))));
        })
    };
    let answer = ask.await;
    replier.await.unwrap();
    assert!(matches!(answer, Answer::Secret(s) if s.as_str() == "pw"));
    assert_eq!(broker.lock().pending.len(), 0);
}

#[tokio::test(start_paused = true)]
async fn unanswered_prompt_times_out_as_cancel_and_is_forgotten() {
    let broker = Arc::new(PromptBroker::default());
    let (channel, _sent) = capture();
    let prompter = ChannelPrompter {
        broker: Arc::clone(&broker),
        channel,
        timeout: Duration::from_secs(5),
    };
    let answer = prompter
        .ask(Prompt::Passphrase {
            key_path: "/k".into(),
        })
        .await;
    assert!(matches!(answer, Answer::Cancel));
    assert_eq!(broker.lock().pending.len(), 0);
}

#[tokio::test]
async fn a_touch_notice_the_key_ended_is_withdrawn_and_an_answered_one_is_not() {
    let broker = Arc::new(PromptBroker::default());
    let (channel, sent) = capture();
    let prompter = ChannelPrompter {
        broker: Arc::clone(&broker),
        channel,
        timeout: PROMPT_TIMEOUT,
    };
    // The security key answered while the notice was up: it is dropped.
    let ask = prompter.ask(Prompt::SecurityKeyTouch {
        key_path: "~/.ssh/id_ed25519_sk".into(),
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(10), ask)
            .await
            .is_err()
    );
    let shown = sent.recv().unwrap();
    assert_eq!(shown["prompt"]["kind"], "securityKeyTouch");
    let gone = sent.recv().unwrap();
    assert_eq!(gone["prompt"]["kind"], "withdrawn");
    assert_eq!(gone["id"], shown["id"]);

    // Answered: nothing is withdrawn.
    let ask = prompter.ask(Prompt::SecurityKeyPin {
        key_path: "~/.ssh/id_ed25519_sk".into(),
    });
    let replier = {
        let broker = Arc::clone(&broker);
        tokio::task::spawn_blocking(move || {
            let request = sent.recv().unwrap();
            assert_eq!(request["prompt"]["kind"], "securityKeyPin");
            let id = PromptId::try_from(request["id"].as_u64().unwrap()).unwrap();
            assert!(broker.reply(id, Answer::Secret(Zeroizing::new("1234".into()))));
            sent
        })
    };
    assert!(matches!(ask.await, Answer::Secret(_)));
    let sent = replier.await.unwrap();
    assert!(sent.try_recv().is_err(), "no withdrawal after an answer");
}

#[tokio::test]
async fn an_abandoned_prompt_is_forgotten() {
    let broker = Arc::new(PromptBroker::default());
    let (channel, _sent) = capture();
    let prompter = ChannelPrompter {
        broker: Arc::clone(&broker),
        channel,
        timeout: PROMPT_TIMEOUT,
    };
    let ask = prompter.ask(Prompt::Password {
        endpoint: "a@db".into(),
    });
    // Poll once so the prompt registers, then drop it like a cancelled connect.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), ask)
            .await
            .is_err()
    );
    assert_eq!(broker.lock().pending.len(), 0);
}

#[test]
fn connect_errors_map_to_stable_codes() {
    let cases = [
        (
            ConnectError::HostKeyChanged {
                host: "h".into(),
                port: 22,
                fingerprint: "f".into(),
                recorded: "r".into(),
            },
            ErrorCode::HostKeyChanged,
        ),
        (
            ConnectError::HostKeyRevoked {
                host: "h".into(),
                port: 22,
                recorded: "r".into(),
            },
            ErrorCode::HostKeyRevoked,
        ),
        (ConnectError::Cancelled, ErrorCode::Cancelled),
        (
            ConnectError::HostKeyRejected {
                host: "h".into(),
                port: 22,
            },
            ErrorCode::Cancelled,
        ),
        (
            ConnectError::AuthFailed {
                endpoint: "e".into(),
                tried: vec!["password".into()],
            },
            ErrorCode::AuthFailed,
        ),
        (
            ConnectError::Refused {
                host: "h".into(),
                port: 22,
            },
            ErrorCode::ConnectFailed,
        ),
        (ConnectError::Protocol("x".into()), ErrorCode::ProtocolError),
        (
            ConnectError::Secret(tauminus_app::vault::SecretError {
                manager: "Vault",
                message: "Error making API request.".into(),
                sign_in: "run vault login",
            }),
            ErrorCode::SecretUnavailable,
        ),
    ];
    for (error, code) in cases {
        assert_eq!(CommandError::from(SshOpenError::Connect(error)).code, code);
    }
}

#[test]
fn server_output_in_an_error_is_short_and_plain() {
    assert_eq!(server_said("  denied\n"), "denied");
    assert_eq!(server_said("a\x1b[31mb\rc"), "a [31mb c");
    let long = "x".repeat(10_000);
    let said = server_said(&long);
    assert_eq!(said.chars().count(), 2 * 1024 + 1);
    assert!(said.ends_with('…'));
}
