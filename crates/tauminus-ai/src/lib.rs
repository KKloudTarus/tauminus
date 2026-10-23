// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The AI assistant's transport: one chat completion from an
//! OpenAI-compatible server (OpenAI, Ollama, `llama.cpp`, LM Studio, vLLM, and
//! the proxies that speak the same API).
//!
//! HTTPS with rustls on ring and the OS's certificate checks. No redirect
//! is followed, so a key never goes to a second address; no proxy is read
//! from the environment; `localhost` is 127.0.0.1 without asking DNS. The
//! answer is read up to [`MAX_BODY`] bytes, and neither the request nor the
//! answer is logged.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Client, StatusCode, redirect};
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::{Deserialize, Serialize};
use tauminus_app::ports::{AiTransport, AiTransportError, BoxFuture, Secret};
use tauminus_domain::{AiEndpoint, AiPrompt};

/// Most of an answer read.
pub const MAX_BODY: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A remote server answers in this, or the request ends.
const TIMEOUT: Duration = Duration::from_secs(60);
/// A local model may be slow to load and to answer.
const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(180);

/// Chat completions over HTTPS.
#[derive(Debug, Clone)]
pub struct OpenAiCompatible {
    tls: Arc<ClientConfig>,
}

impl OpenAiCompatible {
    /// Fails only when the OS's certificate store cannot be read.
    pub fn new() -> Result<Self, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(BuilderVerifierExt::with_platform_verifier)
            .map_err(|e| format!("TLS: {e}"))?
            .with_no_client_auth();
        Ok(Self { tls: Arc::new(tls) })
    }

    fn client(&self, endpoint: &AiEndpoint) -> Result<Client, AiTransportError> {
        let mut builder = Client::builder()
            .tls_backend_preconfigured((*self.tls).clone())
            .redirect(redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(if endpoint.is_loopback() {
                LOOPBACK_TIMEOUT
            } else {
                TIMEOUT
            });
        if endpoint.host() == "localhost" {
            let port = endpoint
                .port()
                .unwrap_or(if endpoint.origin().starts_with("https:") {
                    443
                } else {
                    80
                });
            builder = builder.resolve(
                "localhost",
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
            );
        }
        builder
            .build()
            .map_err(|e| AiTransportError::Unreachable(format!("client: {}", e.without_url())))
    }

    async fn post(
        &self,
        endpoint: &AiEndpoint,
        key: Option<&Secret>,
        prompt: &AiPrompt,
    ) -> Result<String, AiTransportError> {
        let client = self.client(endpoint)?;
        let body = Request {
            model: endpoint.model(),
            messages: [
                Message {
                    role: "system",
                    content: prompt.system(),
                },
                Message {
                    role: "user",
                    content: prompt.user(),
                },
            ],
            stream: false,
        };
        let mut request = client.post(endpoint.completions_url()).json(&body);
        if let Some(key) = key {
            let bearer = Secret::new(format!("Bearer {}", key.as_str()));
            let mut value = HeaderValue::from_str(&bearer)
                .map_err(|_| AiTransportError::Malformed("the key is not a valid header".into()))?;
            value.set_sensitive(true);
            request = request.header(AUTHORIZATION, value);
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| transport_error(&e, endpoint))?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AiTransportError::Unauthorized(status.as_u16()));
        }
        if !status.is_success() {
            return Err(AiTransportError::Status(status.as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| transport_error(&e, endpoint))?
        {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(AiTransportError::Malformed(format!(
                    "longer than {} KiB",
                    MAX_BODY / 1024
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        let parsed: Response = serde_json::from_slice(&bytes)
            .map_err(|_| AiTransportError::Malformed("not a chat completion".into()))?;
        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| AiTransportError::Malformed("no answer in it".into()))
    }
}

/// What a failed request says, without its URL or body.
fn transport_error(error: &reqwest::Error, endpoint: &AiEndpoint) -> AiTransportError {
    if error.is_timeout() {
        AiTransportError::Timeout
    } else if error.is_connect() {
        AiTransportError::Unreachable(endpoint.origin().to_owned())
    } else if error.is_decode() || error.is_body() {
        AiTransportError::Malformed("the connection broke off".into())
    } else {
        AiTransportError::Unreachable(endpoint.origin().to_owned())
    }
}

impl AiTransport for OpenAiCompatible {
    fn complete<'a>(
        &'a self,
        endpoint: &'a AiEndpoint,
        key: Option<&'a Secret>,
        prompt: &'a AiPrompt,
    ) -> BoxFuture<'a, Result<String, AiTransportError>> {
        Box::pin(self.post(endpoint, key, prompt))
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    stream: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Answer,
}

#[derive(Deserialize)]
struct Answer {
    content: Option<String>,
}

#[cfg(test)]
mod tests;
