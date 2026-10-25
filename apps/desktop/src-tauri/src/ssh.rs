// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SSH over IPC: the target the view describes, and the prompts a connect
//! raises (host key, password, passphrase, challenge) with their answers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_app::ports::{Answer, BoxFuture, ConnectError, Credentials, Prompt, Prompter};
use tauminus_app::{Forwards, Login, LoginError, SshOpenError, SshSessions};
use tauminus_domain::{
    AuthMethod, AuthOptions, Endpoint, HostName, IdentityAgent, Route, SshTarget, UserName,
};
use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::ipc::{
    ChannelSink, CommandError, ErrorCode, GridSize, OutputChunk, SessionExit, SessionHandle,
};

/// `user@host:port` as the view sends it.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub(crate) struct EndpointSpec {
    /// Empty means the local user name, as `ssh` does.
    pub(crate) user: String,
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl TryFrom<EndpointSpec> for Endpoint {
    type Error = CommandError;

    fn try_from(spec: EndpointSpec) -> Result<Self, Self::Error> {
        let invalid = |e: tauminus_domain::NameError| {
            CommandError::new(ErrorCode::InvalidArgument, e.to_string())
        };
        let user = if spec.user.is_empty() {
            local_user()?
        } else {
            spec.user
        };
        Endpoint::new(
            UserName::parse(&user).map_err(invalid)?,
            HostName::parse(&spec.host).map_err(invalid)?,
            spec.port,
        )
        .map_err(invalid)
    }
}

/// The name `ssh` would log in as when none is given.
pub(crate) fn local_user() -> Result<String, CommandError> {
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .ok_or_else(|| {
            CommandError::new(
                ErrorCode::InvalidArgument,
                "no user name given and the local one is unknown",
            )
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum RouteSpec {
    Direct,
    ProxyCommand { command: String },
    Socks5 { host: String, port: u16 },
    HttpConnect { host: String, port: u16 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AuthMethodSpec {
    Agent,
    PublicKey,
    KeyboardInteractive,
    Password,
}

impl From<AuthMethod> for AuthMethodSpec {
    fn from(method: AuthMethod) -> Self {
        match method {
            AuthMethod::Agent => Self::Agent,
            AuthMethod::PublicKey => Self::PublicKey,
            AuthMethod::KeyboardInteractive => Self::KeyboardInteractive,
            AuthMethod::Password => Self::Password,
        }
    }
}

impl From<&Route> for RouteSpec {
    fn from(route: &Route) -> Self {
        match route {
            Route::Direct => Self::Direct,
            Route::ProxyCommand(command) => Self::ProxyCommand {
                command: command.clone(),
            },
            Route::Socks5 { host, port } => Self::Socks5 {
                host: host.to_string(),
                port: *port,
            },
            Route::HttpConnect { host, port } => Self::HttpConnect {
                host: host.to_string(),
                port: *port,
            },
        }
    }
}

impl From<&Endpoint> for EndpointSpec {
    fn from(endpoint: &Endpoint) -> Self {
        Self {
            user: endpoint.user.to_string(),
            host: endpoint.host.to_string(),
            port: endpoint.port,
        }
    }
}

impl From<&SshTarget> for SshTargetSpec {
    /// Every field spelled out, so the view can connect without knowing
    /// the defaults.
    fn from(target: &SshTarget) -> Self {
        Self {
            destination: (&target.destination).into(),
            jumps: target.jumps.iter().map(EndpointSpec::from).collect(),
            route: (&target.route).into(),
            methods: Some(
                target
                    .auth
                    .methods
                    .iter()
                    .copied()
                    .map(AuthMethodSpec::from)
                    .collect(),
            ),
            identity_files: target
                .auth
                .identity_files
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            identities_only: target.auth.identities_only,
            identity_agent: target
                .auth
                .identity_agent
                .as_ref()
                .map(IdentityAgent::to_config),
        }
    }
}

impl From<AuthMethodSpec> for AuthMethod {
    fn from(spec: AuthMethodSpec) -> Self {
        match spec {
            AuthMethodSpec::Agent => Self::Agent,
            AuthMethodSpec::PublicKey => Self::PublicKey,
            AuthMethodSpec::KeyboardInteractive => Self::KeyboardInteractive,
            AuthMethodSpec::Password => Self::Password,
        }
    }
}

/// Where and how to connect. Omitted auth fields take the defaults: agent,
/// default key files, keyboard-interactive, then password.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SshTargetSpec {
    pub(crate) destination: EndpointSpec,
    #[serde(default)]
    pub(crate) jumps: Vec<EndpointSpec>,
    pub(crate) route: RouteSpec,
    #[serde(default)]
    pub(crate) methods: Option<Vec<AuthMethodSpec>>,
    #[serde(default)]
    pub(crate) identity_files: Vec<String>,
    #[serde(default)]
    pub(crate) identities_only: bool,
    /// `IdentityAgent`: `none`, `SSH_AUTH_SOCK` or a socket path; absent
    /// is the app's default agent.
    #[serde(default)]
    pub(crate) identity_agent: Option<String>,
}

/// Most auth methods a target can list; there are four kinds.
const MAX_METHODS: usize = 8;
/// Most identity files a target can list, as a sanity bound on input.
const MAX_IDENTITY_FILES: usize = 32;
/// Longest `ProxyCommand` accepted, in bytes.
const MAX_PROXY_COMMAND: usize = 4096;

impl TryFrom<SshTargetSpec> for SshTarget {
    type Error = CommandError;

    fn try_from(spec: SshTargetSpec) -> Result<Self, Self::Error> {
        let invalid = |message: String| CommandError::new(ErrorCode::InvalidArgument, message);
        let host = |h: &str| HostName::parse(h).map_err(|e| invalid(e.to_string()));
        let route = match spec.route {
            RouteSpec::Direct => Route::Direct,
            RouteSpec::ProxyCommand { command } => {
                if command.trim().is_empty() || command.len() > MAX_PROXY_COMMAND {
                    return Err(invalid("ProxyCommand is empty or too long".into()));
                }
                Route::ProxyCommand(command)
            }
            RouteSpec::Socks5 { host: h, port } => Route::Socks5 {
                host: host(&h)?,
                port,
            },
            RouteSpec::HttpConnect { host: h, port } => Route::HttpConnect {
                host: host(&h)?,
                port,
            },
        };
        let mut auth = AuthOptions::default();
        if let Some(methods) = spec.methods {
            if methods.is_empty() || methods.len() > MAX_METHODS {
                return Err(invalid(format!(
                    "between 1 and {MAX_METHODS} auth methods are allowed"
                )));
            }
            auth.methods = methods.into_iter().map(AuthMethod::from).collect();
        }
        if spec.identity_files.len() > MAX_IDENTITY_FILES {
            return Err(invalid(format!(
                "at most {MAX_IDENTITY_FILES} identity files are allowed"
            )));
        }
        auth.identity_files = spec.identity_files.into_iter().map(PathBuf::from).collect();
        auth.identities_only = spec.identities_only;
        auth.identity_agent = spec
            .identity_agent
            .map(|a| IdentityAgent::parse(&a))
            .transpose()
            .map_err(invalid)?;
        let jumps = spec
            .jumps
            .into_iter()
            .map(Endpoint::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        SshTarget::new(spec.destination.try_into()?, jumps, route, auth)
            .map_err(|e| invalid(e.to_string()))
    }
}

/// Identifies one outstanding prompt.
pub(crate) type PromptId = u32;

/// A question for the user, sent on the connect's prompt channel.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptRequest {
    pub(crate) id: PromptId,
    pub(crate) prompt: PromptKind,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum PromptKind {
    UnknownHostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        certificate_expected: bool,
    },
    Password {
        endpoint: String,
    },
    Passphrase {
        key_path: String,
    },
    /// The PIN of the FIDO2 security key the key file names.
    SecurityKeyPin {
        key_path: String,
    },
    /// Touch the security key; withdrawn once it answers.
    SecurityKeyTouch {
        key_path: String,
    },
    /// The question with this id is no longer asked: hide it.
    Withdrawn,
    KeyboardInteractive {
        endpoint: String,
        name: String,
        instructions: String,
        fields: Vec<ChallengeFieldSpec>,
    },
}

#[derive(Debug, Clone, Serialize, Type)]
pub(crate) struct ChallengeFieldSpec {
    pub(crate) text: String,
    pub(crate) echo: bool,
}

impl From<Prompt> for PromptKind {
    fn from(prompt: Prompt) -> Self {
        match prompt {
            Prompt::UnknownHostKey {
                host,
                port,
                algorithm,
                fingerprint,
                certificate_expected,
            } => Self::UnknownHostKey {
                host,
                port,
                algorithm,
                fingerprint,
                certificate_expected,
            },
            Prompt::Password { endpoint } => Self::Password { endpoint },
            Prompt::Passphrase { key_path } => Self::Passphrase { key_path },
            Prompt::SecurityKeyPin { key_path } => Self::SecurityKeyPin { key_path },
            Prompt::SecurityKeyTouch { key_path } => Self::SecurityKeyTouch { key_path },
            Prompt::KeyboardInteractive {
                endpoint,
                name,
                instructions,
                fields,
            } => Self::KeyboardInteractive {
                endpoint,
                name,
                instructions,
                fields: fields
                    .into_iter()
                    .map(|f| ChallengeFieldSpec {
                        text: f.text,
                        echo: f.echo,
                    })
                    .collect(),
            },
        }
    }
}

/// The user's reply to a prompt.
#[derive(Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum AnswerSpec {
    TrustHostKey { remember: bool },
    Secret { value: String },
    Responses { values: Vec<String> },
    Cancel,
}

impl std::fmt::Debug for AnswerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print secrets.
        f.write_str(match self {
            Self::TrustHostKey { .. } => "TrustHostKey",
            Self::Secret { .. } => "Secret",
            Self::Responses { .. } => "Responses",
            Self::Cancel => "Cancel",
        })
    }
}

impl From<AnswerSpec> for Answer {
    fn from(spec: AnswerSpec) -> Self {
        match spec {
            AnswerSpec::TrustHostKey { remember } => Self::TrustHostKey { remember },
            AnswerSpec::Secret { value } => Self::Secret(Zeroizing::new(value)),
            AnswerSpec::Responses { values } => {
                Self::Responses(values.into_iter().map(Zeroizing::new).collect())
            }
            AnswerSpec::Cancel => Self::Cancel,
        }
    }
}

/// How long a prompt waits for the user. Servers drop an unauthenticated
/// connection sooner (OpenSSH `LoginGraceTime` is 120 s), so this only bounds
/// the entry's lifetime.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(600);

/// Routes replies from the view to the connect waiting on them. Each entry
/// lives from `ask` until its reply, its timeout, or the end of the connect,
/// so the map holds at most one entry per connect in progress. O(1) per
/// operation.
#[derive(Debug, Default)]
pub(crate) struct PromptBroker {
    inner: Mutex<BrokerState>,
}

#[derive(Debug, Default)]
struct BrokerState {
    next: PromptId,
    pending: HashMap<PromptId, oneshot::Sender<Answer>>,
}

impl PromptBroker {
    fn lock(&self) -> std::sync::MutexGuard<'_, BrokerState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn register(&self) -> (PromptId, oneshot::Receiver<Answer>) {
        let (tx, rx) = oneshot::channel();
        let mut state = self.lock();
        state.next = state.next.wrapping_add(1);
        let id = state.next;
        state.pending.insert(id, tx);
        (id, rx)
    }

    /// Forgets `id`; true when it was still waiting for its answer.
    fn forget(&self, id: PromptId) -> bool {
        self.lock().pending.remove(&id).is_some()
    }

    /// Delivers `answer`. Returns false when no prompt has that id, because
    /// it was answered, timed out, or never existed.
    pub(crate) fn reply(&self, id: PromptId, answer: Answer) -> bool {
        let sender = self.lock().pending.remove(&id);
        sender.is_some_and(|tx| tx.send(answer).is_ok())
    }

    /// Cancels every outstanding prompt, for a page reload.
    pub(crate) fn cancel_all(&self) -> usize {
        let pending: Vec<_> = self.lock().pending.drain().collect();
        let count = pending.len();
        for (_, tx) in pending {
            let _ = tx.send(Answer::Cancel);
        }
        count
    }
}

/// Asks through one connect's prompt channel.
struct ChannelPrompter {
    broker: Arc<PromptBroker>,
    channel: Channel<PromptRequest>,
    timeout: Duration,
}

impl Prompter for ChannelPrompter {
    fn ask(&self, prompt: Prompt) -> BoxFuture<'_, Answer> {
        Box::pin(async move {
            let (id, answer) = self.broker.register();
            // Forgets the entry however this future ends, including when the
            // connect is abandoned and drops it mid-wait.
            let _entry = Registration {
                broker: &self.broker,
                channel: &self.channel,
                id,
            };
            let request = PromptRequest {
                id,
                prompt: prompt.into(),
            };
            if self.channel.send(request).is_err() {
                return Answer::Cancel;
            }
            match tokio::time::timeout(self.timeout, answer).await {
                Ok(Ok(answer)) => answer,
                _ => Answer::Cancel,
            }
        })
    }
}

struct Registration<'a> {
    broker: &'a PromptBroker,
    channel: &'a Channel<PromptRequest>,
    id: PromptId,
}

impl Drop for Registration<'_> {
    /// Still unanswered (a touch notice the key ended, a connect given
    /// up): the view hides it.
    fn drop(&mut self) {
        if self.broker.forget(self.id) {
            let _ = self.channel.send(PromptRequest {
                id: self.id,
                prompt: PromptKind::Withdrawn,
            });
        }
    }
}

impl From<SshOpenError> for CommandError {
    fn from(error: SshOpenError) -> Self {
        match error {
            SshOpenError::Session(error) => error.into(),
            SshOpenError::Connect(error) => {
                let code = match &error {
                    ConnectError::HostKeyChanged { .. } => ErrorCode::HostKeyChanged,
                    ConnectError::HostKeyRevoked { .. } => ErrorCode::HostKeyRevoked,
                    ConnectError::HostKeyRejected { .. } | ConnectError::Cancelled => {
                        ErrorCode::Cancelled
                    }
                    ConnectError::AuthFailed { .. } => ErrorCode::AuthFailed,
                    ConnectError::Resolve { .. }
                    | ConnectError::Refused { .. }
                    | ConnectError::Timeout { .. }
                    | ConnectError::Proxy(_)
                    | ConnectError::Io(_) => ErrorCode::ConnectFailed,
                    ConnectError::Protocol(_) => ErrorCode::ProtocolError,
                };
                Self::new(code, error.to_string())
            }
        }
    }
}

/// Connects to an SSH host and opens a shell. Questions for the user arrive
/// on `on_prompt`; answer each with `ssh_prompt_reply`.
///
/// A `ProxyCommand` is refused here: the shell would run it, and it must not
/// come from the webview. Saved hosts imported from `~/.ssh/config` connect
/// through `host_connect` instead, which reads theirs from the store.
#[tauri::command]
#[specta::specta]
pub(crate) async fn ssh_open(
    ssh: State<'_, SshSessions>,
    broker: State<'_, Arc<PromptBroker>>,
    target: SshTargetSpec,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
    on_prompt: Channel<PromptRequest>,
) -> Result<SessionHandle, CommandError> {
    if matches!(target.route, RouteSpec::ProxyCommand { .. }) {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "a ProxyCommand can only come from a saved host",
        ));
    }
    let target = SshTarget::try_from(target)?;
    open_target(&ssh, &broker, &target, size, on_output, on_exit, on_prompt).await
}

/// Connects to a saved host, with its settings and its groups' applied.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri passes each IPC argument separately"
)]
pub(crate) async fn host_connect<R: tauri::Runtime>(
    // The sync gate and the forwards, past specta's ten arguments.
    app: tauri::AppHandle<R>,
    ssh: State<'_, SshSessions>,
    broker: State<'_, Arc<PromptBroker>>,
    hosts: State<'_, crate::hosts::Hosts>,
    vault: State<'_, Arc<tauminus_app::vault::Vault>>,
    id: String,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
    on_prompt: Channel<PromptRequest>,
) -> Result<SessionHandle, CommandError> {
    use tauri::Manager as _;
    let gate = app.state::<crate::sync::Gate>();
    let forwards = app.state::<Arc<Forwards>>();
    // Before resolving: an edit that moves the host from here on voids
    // the rules that would start with it on what was resolved.
    let mark = tauminus_domain::HostId::parse(&id)
        .ok()
        .map(|host| forwards.host_mark(&host));
    let login = login(&hosts, &vault, &gate, &id).await?;
    let prompter = prompter(&broker, on_prompt);
    let sink = ChannelSink {
        output: on_output,
        exit: on_exit,
    };
    let session = ssh
        .open_saved(&login, size.try_into()?, sink, Arc::clone(&prompter))
        .await?;
    if let Some(mark) = mark {
        crate::forwards::start_with_host(&hosts, &gate, &forwards, login, mark, prompter);
    }
    SessionHandle::try_from(session)
}

/// A saved host resolved for one login. A host with an identity needs the
/// vault open: the view asks to unlock on `vault_locked` and tries again.
pub(crate) async fn login(
    hosts: &crate::hosts::Hosts,
    vault: &tauminus_app::vault::Vault,
    gate: &crate::sync::Gate,
    id: &str,
) -> Result<Login, CommandError> {
    let invalid =
        |e: &dyn std::fmt::Display| CommandError::new(ErrorCode::InvalidArgument, e.to_string());
    let id = tauminus_domain::HostId::parse(id).map_err(|e| invalid(&e))?;
    let me = UserName::parse(&local_user()?).map_err(|e| invalid(&e))?;
    let login = Login::resolve(hosts.book()?, vault, &id, &me)
        .await
        .map_err(|error| match error {
            LoginError::Vault(error) => error.into(),
            LoginError::Hosts(error) => invalid(&error),
            LoginError::Name(error) => invalid(&error),
        })?;
    // After the read: a sync that saved what was read noted it first.
    gate.check(hosts, &id).await?;
    Ok(login)
}

/// Asks the view, on `channel`, whatever a login needs answered.
pub(crate) fn prompter(
    broker: &Arc<PromptBroker>,
    channel: Channel<PromptRequest>,
) -> Arc<dyn Prompter> {
    Arc::new(ChannelPrompter {
        broker: Arc::clone(broker),
        channel,
        timeout: PROMPT_TIMEOUT,
    })
}

/// Adds an identity's public key to `~/.ssh/authorized_keys` on a saved
/// host, signing in as a connect to that host would, prompts included.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::too_many_arguments,
    reason = "Tauri passes each IPC argument separately"
)]
pub(crate) async fn identity_install(
    ssh: State<'_, SshSessions>,
    broker: State<'_, Arc<PromptBroker>>,
    hosts: State<'_, crate::hosts::Hosts>,
    vault: State<'_, Arc<tauminus_app::vault::Vault>>,
    gate: State<'_, crate::sync::Gate>,
    identity: String,
    host: String,
    on_prompt: Channel<PromptRequest>,
) -> Result<(), CommandError> {
    let invalid = |message: String| CommandError::new(ErrorCode::InvalidArgument, message);
    let line = vault
        .public_key(&identity)
        .await?
        .ok_or_else(|| invalid("this identity has no key to install".into()))?;
    let line =
        tauminus_domain::PublicKeyLine::installable(&line).map_err(|e| invalid(e.to_string()))?;
    let login = login(&hosts, &vault, &gate, &host).await?;
    let outcome = ssh
        .run_script(
            &login.target,
            prompter(&broker, on_prompt),
            login.credentials,
            &tauminus_domain::install_script(&line),
        )
        .await
        .map_err(|e| CommandError::from(SshOpenError::Connect(e)))?;
    match outcome.status {
        Some(0) => Ok(()),
        status => Err(CommandError::new(
            ErrorCode::LaunchFailed,
            format!(
                "the server could not add the key (exit {}): {}",
                status.map_or_else(|| "unknown".into(), |s| s.to_string()),
                server_said(&outcome.output)
            ),
        )),
    }
}

/// The start of a script's output, for an error message: at most 2 KiB,
/// without control characters a server could use to dress it up.
fn server_said(output: &str) -> String {
    const MAX: usize = 2 * 1024;
    let mut text: String = output
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX)
        .collect();
    if output.trim().chars().count() > MAX {
        text.push('…');
    }
    text
}

/// A session's three channels to the view.
struct Channels {
    output: Channel<OutputChunk>,
    exit: Channel<SessionExit>,
    prompt: Channel<PromptRequest>,
}

async fn open_target(
    ssh: &SshSessions,
    broker: &Arc<PromptBroker>,
    target: &SshTarget,
    size: GridSize,
    on_output: Channel<OutputChunk>,
    on_exit: Channel<SessionExit>,
    on_prompt: Channel<PromptRequest>,
) -> Result<SessionHandle, CommandError> {
    let channels = Channels {
        output: on_output,
        exit: on_exit,
        prompt: on_prompt,
    };
    open_target_with(ssh, broker, target, size, channels, Arc::default()).await
}

async fn open_target_with(
    ssh: &SshSessions,
    broker: &Arc<PromptBroker>,
    target: &SshTarget,
    size: GridSize,
    channels: Channels,
    credentials: Arc<Credentials>,
) -> Result<SessionHandle, CommandError> {
    let size = size.try_into()?;
    let prompter = Arc::new(ChannelPrompter {
        broker: Arc::clone(broker),
        channel: channels.prompt,
        timeout: PROMPT_TIMEOUT,
    });
    let sink = ChannelSink {
        output: channels.output,
        exit: channels.exit,
    };
    let id = ssh
        .open_with(target, size, sink, prompter, credentials)
        .await?;
    SessionHandle::try_from(id)
}

/// Answers a prompt. Returns false when the prompt is no longer waiting.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn ssh_prompt_reply(
    broker: State<'_, Arc<PromptBroker>>,
    id: PromptId,
    answer: AnswerSpec,
) -> bool {
    broker.reply(id, answer.into())
}

#[cfg(test)]
mod tests;
