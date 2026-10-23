// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use tauminus_domain::{Ask, AskContext, SessionKind, prompt};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;

/// Serves one request with `response`, and hands back the request as read.
async fn server(response: String) -> (u16, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .and_then(|l| l.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.ok();
        String::from_utf8_lossy(&request).to_string()
    });
    (port, task)
}

fn ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn ask() -> AiPrompt {
    prompt(
        &Ask::Command {
            request: "list files".into(),
        },
        AskContext {
            kind: SessionKind::Local,
            os: "linux",
        },
    )
    .unwrap()
}

fn endpoint(host: &str, port: u16) -> AiEndpoint {
    AiEndpoint::parse(&format!("http://{host}:{port}/v1"), "llama3").unwrap()
}

#[tokio::test]
async fn posts_one_chat_completion_and_reads_its_answer() {
    let (port, request) = server(ok(
        r#"{"choices":[{"message":{"role":"assistant","content":"```sh\nls\n```"}}]}"#,
    ))
    .await;
    let transport = OpenAiCompatible::new().unwrap();
    let key = Secret::new("sk-test".into());
    let answer = transport
        .complete(&endpoint("127.0.0.1", port), Some(&key), &ask())
        .await
        .unwrap();
    assert_eq!(answer, "```sh\nls\n```");
    let request = request.await.unwrap();
    assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(request.contains("authorization: Bearer sk-test\r\n"));
    let body: serde_json::Value =
        serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(body["model"], "llama3");
    assert_eq!(body["stream"], false);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "list files");
}

#[tokio::test]
async fn localhost_is_this_computer_without_dns() {
    let (port, request) = server(ok(r#"{"choices":[{"message":{"content":"hi"}}]}"#)).await;
    let transport = OpenAiCompatible::new().unwrap();
    let answer = transport
        .complete(&endpoint("localhost", port), None, &ask())
        .await
        .unwrap();
    assert_eq!(answer, "hi");
    assert!(
        !request
            .await
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization")
    );
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (port, _request) = server(
        "HTTP/1.1 307 Temporary Redirect\r\nlocation: https://evil.example/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
    )
    .await;
    let transport = OpenAiCompatible::new().unwrap();
    let key = Secret::new("sk-test".into());
    assert_eq!(
        transport
            .complete(&endpoint("127.0.0.1", port), Some(&key), &ask())
            .await,
        Err(AiTransportError::Status(307))
    );
}

#[tokio::test]
async fn refusals_and_odd_answers_are_named_without_the_body() {
    let transport = OpenAiCompatible::new().unwrap();
    let cases = [
        (
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 11\r\nconnection: close\r\n\r\nlist files!".to_owned(),
            AiTransportError::Unauthorized(401),
        ),
        (
            "HTTP/1.1 500 Oops\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned(),
            AiTransportError::Status(500),
        ),
        (
            ok(r#"{"error":"list files"}"#),
            AiTransportError::Malformed("not a chat completion".into()),
        ),
        (
            ok(r#"{"choices":[]}"#),
            AiTransportError::Malformed("no answer in it".into()),
        ),
        (
            ok(&format!(r#"{{"x":"{}"}}"#, "a".repeat(MAX_BODY))),
            AiTransportError::Malformed(format!("longer than {} KiB", MAX_BODY / 1024)),
        ),
    ];
    for (response, error) in cases {
        let (port, _request) = server(response).await;
        let got = transport
            .complete(&endpoint("127.0.0.1", port), None, &ask())
            .await;
        assert_eq!(got, Err(error.clone()));
        assert!(!error.to_string().contains("list files"));
    }
}

#[tokio::test]
async fn a_closed_port_is_unreachable() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let transport = OpenAiCompatible::new().unwrap();
    assert!(matches!(
        transport
            .complete(&endpoint("127.0.0.1", port), None, &ask())
            .await,
        Err(AiTransportError::Unreachable(_))
    ));
}

/// Needs the network: `cargo test -p tauminus-ai -- --ignored`.
#[tokio::test]
#[ignore = "reaches api.openai.com"]
async fn a_real_server_is_reached_over_verified_tls() {
    let transport = OpenAiCompatible::new().unwrap();
    let endpoint = AiEndpoint::parse("https://api.openai.com/v1", "gpt-4o-mini").unwrap();
    assert_eq!(
        transport.complete(&endpoint, None, &ask()).await,
        Err(AiTransportError::Unauthorized(401))
    );
    // A certificate for another name is refused.
    let wrong = AiEndpoint::parse("https://wrong.host.badssl.com", "m").unwrap();
    assert!(matches!(
        transport.complete(&wrong, None, &ask()).await,
        Err(AiTransportError::Unreachable(_))
    ));
}
