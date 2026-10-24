// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tauminus_domain::{
    Endpoint, Host, HostId, HostName, IdentityId, Inventory, Label, SessionKind, SessionMeta,
    UserName,
};
use tokio::sync::mpsc;

use super::*;
use crate::SessionConfig;
use crate::ports::{
    BoxFuture, HostStore, OutputSink, SinkClosed, StoreError, TerminalEvent, TerminalInput,
    TerminalStream, Terminator,
};
use crate::vault::IdentityInput;
use crate::vault::tests::{secret, vault};

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

/// What a session was sent, in order: secrets as their text.
type Typed = Arc<StdMutex<Vec<String>>>;

fn session(
    sessions: &SessionManager,
    meta: SessionMeta,
) -> (SessionId, mpsc::Sender<TerminalEvent>, Typed) {
    let (events_tx, events) = mpsc::channel(64);
    let (input, mut input_rx) = mpsc::channel(64);
    let typed: Typed = Arc::default();
    let seen = Arc::clone(&typed);
    tokio::spawn(async move {
        while let Some(message) = input_rx.recv().await {
            if let TerminalInput::Secret(secret) = message {
                seen.lock().unwrap().push(secret.to_string());
            }
        }
    });
    let id = sessions.attach(
        sessions.reserve().unwrap(),
        TerminalStream {
            input,
            events,
            terminator: Box::new(Nothing),
        },
        Quiet,
        meta,
    );
    (id, events_tx, typed)
}

struct World {
    fill: Fill,
    sessions: Arc<SessionManager>,
    vault: Arc<Vault>,
    hosts: Arc<HostBook>,
    host: HostId,
    identity: IdentityId,
}

/// A host `db.example.com` whose login identity `deploy` holds `password`
/// (or none).
async fn world(password: Option<&str>) -> World {
    let (_, vault) = vault();
    vault.create(secret("correct horse")).await.unwrap();
    let identity = vault
        .save_identity(IdentityInput {
            name: "deploy".into(),
            user: Some("deploy".into()),
            password: password.map(secret),
            ..IdentityInput::default()
        })
        .await
        .unwrap();
    let identity = IdentityId::parse(&identity).unwrap();
    let hosts = Arc::new(
        HostBook::open(Arc::new(MemoryHosts::default()) as Arc<dyn HostStore>)
            .await
            .unwrap(),
    );
    let host = HostId::parse("db").unwrap();
    let mut saved = Host::new(
        host.clone(),
        Label::parse("db").unwrap(),
        HostName::parse("db.example.com").unwrap(),
    );
    saved.settings.identity = Some(identity.clone());
    hosts.edit(|i| i.put_host(saved)).await.unwrap();
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    World {
        fill: Fill::new(
            Arc::clone(&sessions),
            Arc::clone(&hosts),
            Arc::clone(&vault),
        ),
        sessions,
        vault,
        hosts,
        host,
        identity,
    }
}

impl World {
    /// The session meta a saved host's session gets: as it logged in.
    fn saved(&self, kind: SessionKind) -> SessionMeta {
        SessionMeta {
            kind,
            host: Some(SavedHost {
                id: self.host.clone(),
                identity: Some(self.identity.clone()),
                login: Endpoint::new(
                    UserName::parse("deploy").unwrap(),
                    HostName::parse("db.example.com").unwrap(),
                    22,
                )
                .unwrap(),
            }),
            transfers: false,
        }
    }
}

/// Sends `text` as output and waits for the session to have it.
async fn show(
    world: &World,
    id: SessionId,
    events: &mpsc::Sender<TerminalEvent>,
    text: &str,
) -> u64 {
    let before = world.sessions.received(id).unwrap();
    events
        .send(TerminalEvent::Output(text.as_bytes().to_vec()))
        .await
        .unwrap();
    let want = before + text.len() as u64;
    for _ in 0..200 {
        if world.sessions.received(id).unwrap() == want {
            return want;
        }
        settle().await;
    }
    panic!("output never arrived");
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn types_the_host_s_own_login_password_at_its_prompt_without_enter() {
    let world = world(Some("s3cret")).await;
    let (id, events, typed) = session(&world.sessions, world.saved(SessionKind::Ssh));
    // No prompt yet: nothing offered, nothing typed.
    assert!(matches!(
        world.fill.offer(id).await,
        Err(FillError::NoPrompt)
    ));
    let seen = show(
        &world,
        id,
        &events,
        "$ sudo ls\r\n[sudo] password for deploy: ",
    )
    .await;
    assert_eq!(
        world.fill.offer(id).await.unwrap(),
        Offer {
            identity: Some("deploy".into()),
            login: "deploy@db.example.com".into(),
        }
    );
    world.fill.type_password(id, seen).await.unwrap();
    // Once per prompt.
    assert!(matches!(
        world.fill.type_password(id, seen).await,
        Err(FillError::Stale)
    ));
    settle().await;
    assert_eq!(typed.lock().unwrap().as_slice(), ["s3cret"]);
}

#[tokio::test]
async fn a_prompt_that_names_another_user_or_machine_gets_nothing() {
    let world = world(Some("s3cret")).await;
    let (id, events, typed) = session(&world.sessions, world.saved(SessionKind::Ssh));
    for prompt in [
        "[sudo] password for root: ",
        "\r\ndeploy@other.example's password: ",
        "\r\n(deploy@other.example) Password: ",
        "\r\nPassword: ",
        "\r\nEnter passphrase for key 'id_ed25519': ",
    ] {
        let seen = show(&world, id, &events, prompt).await;
        assert!(
            matches!(world.fill.offer(id).await, Err(FillError::NoPrompt)),
            "{prompt:?}"
        );
        assert!(matches!(
            world.fill.type_password(id, seen).await,
            Err(FillError::NoPrompt)
        ));
    }
    settle().await;
    assert!(typed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nothing_goes_to_a_session_that_is_not_a_saved_host_s_or_runs_telnet() {
    let world = world(Some("s3cret")).await;
    let (local, events, typed) = session(&world.sessions, SessionMeta::local());
    let seen = show(&world, local, &events, "[sudo] password for deploy: ").await;
    assert!(matches!(
        world.fill.offer(local).await,
        Err(FillError::NoHost)
    ));
    assert!(matches!(
        world.fill.type_password(local, seen).await,
        Err(FillError::NoHost)
    ));
    let (telnet, events, _) = session(&world.sessions, world.saved(SessionKind::Telnet));
    let seen = show(&world, telnet, &events, "[sudo] password for deploy: ").await;
    assert!(matches!(
        world.fill.type_password(telnet, seen).await,
        Err(FillError::Telnet)
    ));
    settle().await;
    assert!(typed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_edited_host_does_not_send_its_password_to_a_session_opened_before() {
    let world = world(Some("s3cret")).await;
    let (id, events, typed) = session(&world.sessions, world.saved(SessionKind::Ssh));
    let seen = show(&world, id, &events, "[sudo] password for deploy: ").await;
    // The host now points somewhere else; this session is still on the old server.
    world
        .hosts
        .edit(|i| {
            let mut host = i.host(&world.host).unwrap().clone();
            host.address = HostName::parse("new.example.com").unwrap();
            i.put_host(host)
        })
        .await
        .unwrap();
    assert!(matches!(
        world.fill.offer(id).await,
        Err(FillError::Changed)
    ));
    assert!(matches!(
        world.fill.type_password(id, seen).await,
        Err(FillError::Changed)
    ));
    settle().await;
    assert!(typed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn output_the_view_has_not_seen_stops_it() {
    let world = world(Some("s3cret")).await;
    let (id, events, typed) = session(&world.sessions, world.saved(SessionKind::Ssh));
    let seen = show(&world, id, &events, "[sudo] password for deploy: ").await;
    assert!(matches!(
        world.fill.type_password(id, seen - 4).await,
        Err(FillError::Stale)
    ));
    world.fill.type_password(id, seen).await.unwrap();
    settle().await;
    assert_eq!(typed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_locked_vault_offers_without_a_name_types_nothing_and_stays_idle() {
    let world = world(Some("s3cret")).await;
    let (id, events, typed) = session(&world.sessions, world.saved(SessionKind::Ssh));
    let seen = show(&world, id, &events, "[sudo] password for deploy: ").await;
    world.vault.lock().await;
    assert_eq!(world.fill.offer(id).await.unwrap().identity, None);
    assert!(matches!(
        world.fill.type_password(id, seen).await,
        Err(FillError::Vault(VaultError::Locked))
    ));
    settle().await;
    assert!(typed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_offer_does_not_count_as_use_of_the_vault() {
    let world = world(Some("s3cret")).await;
    let (id, events, _) = session(&world.sessions, world.saved(SessionKind::Ssh));
    show(&world, id, &events, "[sudo] password for deploy: ").await;
    crate::vault::tests::age(&world.vault).await;
    world.fill.offer(id).await.unwrap();
    // Idle as long as it was: the next look finds it locked.
    assert_eq!(world.vault.identities().await, Err(VaultError::Locked));
}

#[tokio::test]
async fn an_identity_without_a_password_is_not_offered() {
    let world = world(None).await;
    let (id, events, _typed) = session(&world.sessions, world.saved(SessionKind::Mosh));
    let seen = show(&world, id, &events, "[sudo] password for deploy: ").await;
    assert!(matches!(
        world.fill.offer(id).await,
        Err(FillError::NoPassword)
    ));
    assert!(matches!(
        world.fill.type_password(id, seen).await,
        Err(FillError::NoPassword)
    ));
}
