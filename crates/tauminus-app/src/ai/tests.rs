// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The v0.4 exit criterion: the assistant never sends terminal output off
//! the machine without the host's leave. A recording transport stands in
//! for every server; output carries a sentinel no request may hold unless
//! the session lets the assistant read it.

use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tauminus_domain::{
    Endpoint, Group, GroupId, Host, HostId, HostName, Inventory, Label, SavedHost, Settings,
    UserName,
};
use tokio::sync::{Notify, mpsc};

use super::*;
use crate::SessionConfig;
use crate::ports::{
    BoxFuture, HostStore, OutputSink, SinkClosed, StoreError, TerminalEvent, TerminalStream,
    Terminator,
};
use crate::vault::tests::{secret, vault};

const SENTINEL: &str = "SENTINEL-7f3a";
const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

#[derive(Default)]
struct MemoryHosts(StdMutex<Option<Inventory>>);

impl HostStore for MemoryHosts {
    fn load(&self) -> BoxFuture<'_, Result<Inventory, StoreError>> {
        let inventory = self.0.lock().unwrap().clone().unwrap_or_default();
        Box::pin(async move { Ok(inventory) })
    }

    fn save<'a>(&'a self, inventory: &'a Inventory) -> BoxFuture<'a, Result<(), StoreError>> {
        *self.0.lock().unwrap() = Some(inventory.clone());
        Box::pin(async { Ok(()) })
    }

    fn new_id(&self) -> String {
        "h".into()
    }
}

#[derive(Default)]
struct MemorySettings(StdMutex<Option<AiSettings>>);

impl AiSettingsStore for MemorySettings {
    fn load(&self) -> BoxFuture<'_, Result<AiSettings, StoreError>> {
        let settings = self.0.lock().unwrap().clone().unwrap_or_default();
        Box::pin(async move { Ok(settings) })
    }

    fn save<'a>(&'a self, settings: &'a AiSettings) -> BoxFuture<'a, Result<(), StoreError>> {
        *self.0.lock().unwrap() = Some(settings.clone());
        Box::pin(async { Ok(()) })
    }
}

/// One request as a server would get it.
#[derive(Debug, Clone)]
struct Sent {
    url: String,
    key: Option<String>,
    body: String,
}

/// Records every request and answers with `answer`; holds each answer
/// back while `hold` is set.
#[derive(Default)]
struct Recorder {
    sent: StdMutex<Vec<Sent>>,
    answer: StdMutex<String>,
    hold: StdMutex<Option<Arc<Notify>>>,
}

impl Recorder {
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn answer(&self, text: &str) {
        *self.answer.lock().unwrap() = text.to_owned();
    }
}

impl AiTransport for Recorder {
    fn complete<'a>(
        &'a self,
        endpoint: &'a AiEndpoint,
        key: Option<&'a Secret>,
        prompt: &'a AiPrompt,
    ) -> BoxFuture<'a, Result<String, AiTransportError>> {
        self.sent.lock().unwrap().push(Sent {
            url: endpoint.completions_url(),
            key: key.map(|k| k.as_str().to_owned()),
            body: format!("{}\n{}", prompt.system(), prompt.user()),
        });
        let hold = self.hold.lock().unwrap().clone();
        let answer = self.answer.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.notified().await;
            }
            Ok(answer)
        })
    }
}

struct Quiet;

impl OutputSink for Quiet {
    fn send_output(&self, _batch: Vec<u8>) -> Result<(), SinkClosed> {
        Ok(())
    }

    fn send_exit(&self, _code: Option<u32>) {}
}

struct Nothing;

impl Terminator for Nothing {
    fn terminate(&self) {}
}

struct World {
    assistant: Assistant,
    sessions: Arc<SessionManager>,
    hosts: Arc<HostBook>,
    vault: Arc<Vault>,
    transport: Arc<Recorder>,
}

/// Host `db` (db.example.com) in group `team`, neither setting the leave,
/// and the assistant on, asking `https://api.example/v1`.
async fn world() -> World {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let hosts = Arc::new(
        HostBook::open(Arc::new(MemoryHosts::default()) as Arc<dyn HostStore>)
            .await
            .unwrap(),
    );
    let team = Group {
        id: GroupId::parse("team").unwrap(),
        name: Label::parse("team").unwrap(),
        parent: None,
        settings: Settings::default(),
    };
    let mut db = Host::new(
        HostId::parse("db").unwrap(),
        Label::parse("db").unwrap(),
        HostName::parse("db.example.com").unwrap(),
    );
    db.group = Some(GroupId::parse("team").unwrap());
    hosts
        .edit(|i| {
            i.put_group(team)?;
            i.put_host(db)
        })
        .await
        .unwrap();
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let transport = Arc::new(Recorder::default());
    transport.answer("It failed.\n```sh\nls -la\n```");
    let assistant = Assistant::new(
        Arc::clone(&sessions),
        Arc::clone(&hosts),
        Arc::clone(&vault),
        Arc::new(MemorySettings::default()),
        Arc::clone(&transport) as Arc<dyn AiTransport>,
    );
    assistant
        .configure(AiSettingsInput {
            enabled: true,
            base_url: "https://api.example/v1".into(),
            model: "m".into(),
            local_output: false,
        })
        .await
        .unwrap();
    World {
        assistant,
        sessions,
        hosts,
        vault,
        transport,
    }
}

fn saved(kind: SessionKind, address: &str) -> SessionMeta {
    SessionMeta {
        kind,
        host: Some(SavedHost {
            id: HostId::parse("db").unwrap(),
            identity: None,
            login: Endpoint::new(
                UserName::parse("ops").unwrap(),
                HostName::parse(address).unwrap(),
                22,
            )
            .unwrap(),
        }),
    }
}

impl World {
    /// A session with `meta` that has shown the sentinel and an AWS key.
    async fn session(&self, meta: SessionMeta) -> SessionId {
        let (events_tx, events) = mpsc::channel(8);
        let (input, mut input_rx) = mpsc::channel(8);
        tokio::spawn(async move { while input_rx.recv().await.is_some() {} });
        let id = self.sessions.attach(
            self.sessions.reserve().unwrap(),
            TerminalStream {
                input,
                events,
                terminator: Box::new(Nothing),
            },
            Quiet,
            meta,
        );
        let text = format!("$ make\r\n{SENTINEL}: error 2\r\nexport AWS_KEY={AWS_KEY}\r\n$ ");
        let want = text.len() as u64;
        events_tx
            .send(TerminalEvent::Output(text.into_bytes()))
            .await
            .unwrap();
        // Kept open, so the session stays.
        std::mem::forget(events_tx);
        for _ in 0..200 {
            if self.sessions.received(id).unwrap() == want {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("output never arrived");
    }

    async fn leave(&self, group: Option<bool>, host: Option<bool>) {
        self.hosts
            .edit(|i| {
                let mut team = i
                    .groups()
                    .find(|g| g.id.as_str() == "team")
                    .unwrap()
                    .clone();
                team.settings.ai_output = group;
                i.put_group(team)?;
                let mut db = i.hosts().find(|h| h.id.as_str() == "db").unwrap().clone();
                db.settings.ai_output = host;
                i.put_host(db)
            })
            .await
            .unwrap();
    }

    fn leaked(&self) -> bool {
        self.transport
            .sent()
            .iter()
            .any(|s| s.body.contains(SENTINEL))
    }
}

#[tokio::test]
async fn without_the_host_s_leave_no_output_leaves_the_machine() {
    let world = world().await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    // A command request goes, and holds only what was typed.
    let reply = world.assistant.ask(id, "list files").await.unwrap();
    assert_eq!(reply.command.as_deref(), Some("ls -la"));
    let sent = world.transport.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].body.ends_with("\nlist files"));
    assert!(!sent[0].body.contains("db.example.com") && !sent[0].body.contains("ops"));
    assert!(!world.leaked());
}

#[tokio::test]
async fn a_group_s_leave_lets_output_go_with_secrets_hidden() {
    let world = world().await;
    world.leave(Some(true), None).await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let preview = world.assistant.preview(id).await.unwrap();
    assert!(preview.text.contains(SENTINEL));
    assert!(!preview.text.contains(AWS_KEY));
    let reply = world.assistant.explain(id, preview.id).await.unwrap();
    assert_eq!(reply.text, "It failed.\n```sh\nls -la\n```");
    let sent = world.transport.sent();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].body.contains(&preview.text),
        "what was shown is what went"
    );
    assert!(!sent[0].body.contains(AWS_KEY));
    // A preview is sent once.
    assert!(matches!(
        world.assistant.explain(id, preview.id).await,
        Err(AssistantError::NoPreview)
    ));
}

#[tokio::test]
async fn the_host_s_own_no_wins_over_its_group() {
    let world = world().await;
    world.leave(Some(true), Some(false)).await;
    let id = world
        .session(saved(SessionKind::Mosh, "db.example.com"))
        .await;
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn leave_withdrawn_after_the_preview_sends_nothing() {
    let world = world().await;
    world.leave(None, Some(true)).await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let preview = world.assistant.preview(id).await.unwrap();
    world.leave(None, None).await;
    assert!(matches!(
        world.assistant.explain(id, preview.id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn a_session_to_an_address_the_host_no_longer_has_sends_nothing() {
    let world = world().await;
    world.leave(None, Some(true)).await;
    // Logged in to the old address; the host was edited since.
    let id = world
        .session(saved(SessionKind::Ssh, "old.example.com"))
        .await;
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    // A deleted host lets nothing go either.
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    world
        .hosts
        .edit(|i| i.remove_host(&HostId::parse("db").unwrap()).map(|_| ()))
        .await
        .unwrap();
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn telnet_serial_and_ad_hoc_sessions_never_send_output() {
    let world = world().await;
    world.leave(Some(true), Some(true)).await;
    let telnet = world
        .session(saved(SessionKind::Telnet, "db.example.com"))
        .await;
    let adhoc = world.session(SessionMeta::of(SessionKind::Ssh)).await;
    let serial = world.session(SessionMeta::of(SessionKind::Serial)).await;
    for id in [telnet, adhoc, serial] {
        assert!(matches!(
            world.assistant.preview(id).await,
            Err(AssistantError::OutputNotAllowed)
        ));
    }
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn a_local_shell_sends_output_only_with_its_own_setting() {
    let world = world().await;
    let id = world.session(SessionMeta::of(SessionKind::Local)).await;
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    world
        .assistant
        .configure(AiSettingsInput {
            enabled: true,
            base_url: "https://api.example/v1".into(),
            model: "m".into(),
            local_output: true,
        })
        .await
        .unwrap();
    let preview = world.assistant.preview(id).await.unwrap();
    world.assistant.explain(id, preview.id).await.unwrap();
    assert!(world.leaked());
}

#[tokio::test]
async fn a_loopback_server_needs_the_same_leave() {
    let world = world().await;
    world
        .assistant
        .configure(AiSettingsInput {
            enabled: true,
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "llama3".into(),
            local_output: false,
        })
        .await
        .unwrap();
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::OutputNotAllowed)
    ));
    world.assistant.ask(id, "free disk").await.unwrap();
    assert_eq!(
        world.transport.sent()[0].url,
        "http://127.0.0.1:11434/v1/chat/completions"
    );
    assert!(!world.leaked());
}

#[tokio::test]
async fn off_asks_nothing_at_all() {
    let world = world().await;
    world.leave(Some(true), None).await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    world
        .assistant
        .configure(AiSettingsInput {
            enabled: false,
            base_url: "https://api.example/v1".into(),
            model: "m".into(),
            local_output: true,
        })
        .await
        .unwrap();
    assert!(matches!(
        world.assistant.ask(id, "x").await,
        Err(AssistantError::Off)
    ));
    assert!(matches!(
        world.assistant.preview(id).await,
        Err(AssistantError::Off)
    ));
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn the_key_goes_only_to_the_origin_it_was_saved_for() {
    let world = world().await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let settings = world
        .assistant
        .set_key(secret("sk-test"), "https://api.example/v1")
        .await
        .unwrap();
    assert!(settings.has_key());
    world.assistant.ask(id, "x").await.unwrap();
    assert_eq!(world.transport.sent()[0].key.as_deref(), Some("sk-test"));
    // A new address forgets the key: it is not sent there.
    let settings = world
        .assistant
        .configure(AiSettingsInput {
            enabled: true,
            base_url: "https://evil.example/v1".into(),
            model: "m".into(),
            local_output: false,
        })
        .await
        .unwrap();
    assert!(!settings.has_key());
    assert!(world.vault.ai_key().await.unwrap().is_none());
    world.assistant.ask(id, "x").await.unwrap();
    assert_eq!(world.transport.sent()[1].key, None);
    // A path change on the same origin keeps it.
    world
        .assistant
        .set_key(secret("sk-2"), "https://evil.example/v1")
        .await
        .unwrap();
    let settings = world
        .assistant
        .configure(AiSettingsInput {
            enabled: true,
            base_url: "https://evil.example/v2".into(),
            model: "other".into(),
            local_output: false,
        })
        .await
        .unwrap();
    assert!(settings.has_key());
    assert!(matches!(
        world
            .assistant
            .set_key(secret("bad key\r\nX: y"), "https://evil.example/v2")
            .await,
        Err(AssistantError::Key)
    ));
}

#[tokio::test]
async fn with_the_vault_locked_a_saved_key_waits_for_the_unlock() {
    let world = world().await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    world
        .assistant
        .set_key(secret("sk-test"), "https://api.example/v1")
        .await
        .unwrap();
    world.vault.lock().await;
    assert!(matches!(
        world.assistant.ask(id, "x").await,
        Err(AssistantError::Vault(VaultError::Locked))
    ));
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn a_suggested_command_is_checked_by_the_guard_on_any_host() {
    let world = world().await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    world
        .transport
        .answer("```sh\nrm -rf / --no-preserve-root\n```");
    let reply = world.assistant.ask(id, "clean up").await.unwrap();
    assert_eq!(
        reply.command.as_deref(),
        Some("rm -rf / --no-preserve-root")
    );
    assert!(
        !reply.findings.is_empty(),
        "the host has no guard turned on"
    );
    let local = world.session(SessionMeta::of(SessionKind::Local)).await;
    let reply = world.assistant.ask(local, "clean up").await.unwrap();
    assert!(
        !reply.findings.is_empty(),
        "a local shell has no guard of its own"
    );
}

#[tokio::test]
async fn one_request_at_a_time_per_pane() {
    let world = world().await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let hold = Arc::new(Notify::new());
    *world.transport.hold.lock().unwrap() = Some(Arc::clone(&hold));
    let world = Arc::new(world);
    let first = {
        let world = Arc::clone(&world);
        tokio::spawn(async move { world.assistant.ask(id, "one").await })
    };
    while world.transport.sent().is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        world.assistant.ask(id, "two").await,
        Err(AssistantError::Busy)
    ));
    hold.notify_one();
    first.await.unwrap().unwrap();
    *world.transport.hold.lock().unwrap() = None;
    world.assistant.ask(id, "three").await.unwrap();
}

#[tokio::test]
async fn a_key_typed_for_an_unsaved_address_is_refused() {
    let world = world().await;
    assert!(matches!(
        world
            .assistant
            .set_key(secret("sk-b"), "https://b.example/v1")
            .await,
        Err(AssistantError::KeyOrigin)
    ));
    assert!(world.vault.ai_key().await.unwrap().is_none());
}

#[tokio::test]
async fn an_explanation_goes_only_to_the_server_its_preview_named() {
    let world = world().await;
    world.leave(None, Some(true)).await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let preview = world.assistant.preview(id).await.unwrap();
    world
        .assistant
        .configure(AiSettingsInput {
            enabled: true,
            base_url: "https://other.example/v1".into(),
            model: "m".into(),
            local_output: false,
        })
        .await
        .unwrap();
    assert!(matches!(
        world.assistant.explain(id, preview.id).await,
        Err(AssistantError::ServerChanged)
    ));
    assert!(world.transport.sent().is_empty());
}

#[tokio::test]
async fn an_explanation_waits_out_a_locked_vault_and_a_busy_pane() {
    let world = world().await;
    world.leave(None, Some(true)).await;
    world
        .assistant
        .set_key(secret("sk-test"), "https://api.example/v1")
        .await
        .unwrap();
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let preview = world.assistant.preview(id).await.unwrap();
    world.vault.lock().await;
    assert!(matches!(
        world.assistant.explain(id, preview.id).await,
        Err(AssistantError::Vault(VaultError::Locked))
    ));
    world.vault.unlock(secret("correct horse")).await.unwrap();
    world.assistant.explain(id, preview.id).await.unwrap();
    assert_eq!(world.transport.sent().len(), 1);
    // Sent: gone.
    assert!(matches!(
        world.assistant.explain(id, preview.id).await,
        Err(AssistantError::NoPreview)
    ));
}

#[tokio::test]
async fn an_explanation_sent_while_the_pane_is_busy_can_be_sent_again() {
    let world = world().await;
    world.leave(None, Some(true)).await;
    let id = world
        .session(saved(SessionKind::Ssh, "db.example.com"))
        .await;
    let preview = world.assistant.preview(id).await.unwrap();
    let hold = Arc::new(Notify::new());
    *world.transport.hold.lock().unwrap() = Some(Arc::clone(&hold));
    let world = Arc::new(world);
    let asking = {
        let world = Arc::clone(&world);
        tokio::spawn(async move { world.assistant.ask(id, "one").await })
    };
    while world.transport.sent().is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        world.assistant.explain(id, preview.id).await,
        Err(AssistantError::Busy)
    ));
    hold.notify_one();
    asking.await.unwrap().unwrap();
    *world.transport.hold.lock().unwrap() = None;
    world.assistant.explain(id, preview.id).await.unwrap();
}
