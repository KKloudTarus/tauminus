// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Agent forwarding: when the server opens an `auth-agent@openssh.com`
//! channel, its bytes go to the local agent and back, as `ssh -A` does.
//! The agent signs; no key leaves this machine.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use russh::Channel;
use russh::client::Msg;

use crate::auth::AgentSource;

/// Most agent channels open at once on one connection. A server needs one
/// per `ssh` or `git` it runs at a time; more is a server that never closes
/// them.
pub(crate) const MAX_CHANNELS: usize = 16;

/// Lets a server reach the local agent over channels it opens.
#[derive(Debug, Clone)]
pub(crate) struct AgentForward {
    source: AgentSource,
    open: Arc<AtomicUsize>,
}

impl AgentForward {
    pub(crate) fn new(source: AgentSource) -> Self {
        Self {
            source,
            open: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Takes a slot for one more channel, or `None` at the limit.
    pub(crate) fn reserve(&self) -> Option<Slot> {
        let taken = self.open.fetch_add(1, Ordering::AcqRel);
        if taken >= MAX_CHANNELS {
            self.open.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(Arc::clone(&self.open)))
    }

    /// Relays `channel` to the agent until either side closes, on its own
    /// task. The slot is freed when the relay ends.
    pub(crate) fn relay(&self, channel: Channel<Msg>, slot: Slot) {
        let source = self.source.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let mut remote = channel.into_stream();
            let Some(mut agent) = agent_stream(&source).await else {
                tracing::warn!("a forwarded agent request found no local agent");
                return;
            };
            if let Err(error) = relay_requests(&mut remote, &mut agent).await {
                tracing::debug!(%error, "agent forwarding channel ended");
            }
        });
    }
}

/// Largest agent message relayed, as OpenSSH's `AGENT_MAX_LEN`.
const MAX_MESSAGE: usize = 256 * 1024;
/// What a server may ask the agent: its keys (11), a signature (13), and
/// one extension (27), `session-bind@openssh.com`, which binds the agent
/// to the session as OpenSSH 8.9 and later send. Adding or removing keys,
/// locking, other extensions (Pageant's `add-ppk` adds a key), and
/// anything not an agent message are answered with `SSH_AGENT_FAILURE`
/// (5) and never reach the local socket.
const RELAYED: [u8; 2] = [11, 13];
const EXTENSION: u8 = 27;
const SESSION_BIND: &[u8] = b"session-bind@openssh.com";
const FAILURE: [u8; 5] = [0, 0, 0, 1, 5];

/// Relays agent requests from `remote`, one at a time as the protocol
/// goes, each with the agent's answer. O(n) in the bytes relayed; holds
/// one message, at most 256 KiB, at a time.
async fn relay_requests<R, A>(remote: &mut R, agent: &mut A) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    A: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    loop {
        let Some(request) = read_message(remote).await? else {
            return Ok(());
        };
        if relayed(&request) {
            agent.write_all(&request).await?;
            let answer = read_message(agent)
                .await?
                .ok_or(std::io::ErrorKind::UnexpectedEof)?;
            remote.write_all(&answer).await?;
        } else {
            remote.write_all(&FAILURE).await?;
        }
    }
}

/// Whether `message` (length included) may reach the agent.
fn relayed(message: &[u8]) -> bool {
    match message.get(4) {
        Some(kind) if RELAYED.contains(kind) => true,
        Some(&EXTENSION) => {
            // The extension's name, an SSH string after the type.
            let name = message
                .get(5..9)
                .and_then(|len| <[u8; 4]>::try_from(len).ok())
                .map(|len| u32::from_be_bytes(len) as usize)
                .and_then(|len| message.get(9..9 + len));
            name == Some(SESSION_BIND)
        }
        _ => false,
    }
}

/// One length-prefixed agent message, length included; `None` at a clean
/// end before one starts.
async fn read_message<R: tokio::io::AsyncRead + Unpin>(
    from: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;
    let mut length = [0u8; 4];
    match from.read_exact(&mut length).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let size = u32::from_be_bytes(length) as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not an agent message",
        ));
    }
    let mut message = Vec::with_capacity(4 + size);
    message.extend_from_slice(&length);
    message.resize(4 + size, 0);
    from.read_exact(&mut message[4..]).await?;
    Ok(Some(message))
}

/// One open agent channel; dropping it frees the slot.
#[derive(Debug)]
pub(crate) struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

trait Pipe: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Pipe for T {}

#[cfg(unix)]
async fn agent_stream(source: &AgentSource) -> Option<Box<dyn Pipe>> {
    let stream = match source {
        AgentSource::Environment => {
            tokio::net::UnixStream::connect(std::env::var_os("SSH_AUTH_SOCK")?)
                .await
                .ok()?
        }
        AgentSource::Path(path) => crate::auth::own_socket(path).await?,
        AgentSource::Pageant | AgentSource::None => return None,
    };
    Some(Box::new(stream))
}

/// The same pipes, with the same owner checks, that signing uses.
#[cfg(windows)]
async fn agent_stream(source: &AgentSource) -> Option<Box<dyn Pipe>> {
    for candidate in crate::windows_agent::candidates(source, std::env::var_os("SSH_AUTH_SOCK")) {
        if let Ok(pipe) = crate::windows_agent::connect(&candidate).await {
            return Some(Box::new(pipe));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One agent message of `kind` with `body`.
    fn message(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut m = u32::try_from(body.len() + 1)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        m.push(kind);
        m.extend_from_slice(body);
        m
    }

    #[tokio::test]
    async fn only_listing_signing_and_extensions_reach_the_agent() {
        let (mut server, mut remote) = tokio::io::duplex(1 << 16);
        let (mut agent_end, mut agent) = tokio::io::duplex(1 << 16);
        let relay = tokio::spawn(async move { relay_requests(&mut remote, &mut agent).await });
        // Adding a key (17), locking (22), and an extension other than
        // session-bind are answered here.
        let name = b"add-ppk@putty.projects.tartarus.org";
        let mut add_ppk = u32::try_from(name.len()).unwrap().to_be_bytes().to_vec();
        add_ppk.extend_from_slice(name);
        for (kind, body) in [(17, &b"x"[..]), (22, b"x"), (27, &add_ppk)] {
            server.write_all(&message(kind, body)).await.unwrap();
            let mut answer = [0u8; 5];
            server.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer, FAILURE, "{kind}");
        }
        // session-bind and a signature go through.
        let mut bind = u32::try_from(SESSION_BIND.len())
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bind.extend_from_slice(SESSION_BIND);
        for (kind, body) in [(27, bind.as_slice()), (13, b"sign this")] {
            server.write_all(&message(kind, body)).await.unwrap();
            let mut seen = vec![0u8; 5 + body.len()];
            agent_end.read_exact(&mut seen).await.unwrap();
            assert_eq!(seen, message(kind, body));
            agent_end.write_all(&message(6, b"")).await.unwrap();
            let mut answer = [0u8; 5];
            server.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer.to_vec(), message(6, b""));
        }
        // A request for the keys goes through, and its answer comes back.
        server.write_all(&message(11, b"")).await.unwrap();
        let mut seen = vec![0u8; 5];
        agent_end.read_exact(&mut seen).await.unwrap();
        assert_eq!(seen, message(11, b""), "only the listing reached the agent");
        agent_end.write_all(&message(12, b"keys")).await.unwrap();
        let mut answer = vec![0u8; 9];
        server.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer, message(12, b"keys"));
        // Bytes of another protocol end the channel before any reach it.
        server
            .write_all(b"\0AUTH EXTERNAL 31303030\r\n")
            .await
            .unwrap();
        assert!(relay.await.unwrap().is_err());
        drop(server);
        let mut rest = Vec::new();
        agent_end.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "{rest:?}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_agent_socket_of_another_user_is_refused() {
        let dir = std::env::temp_dir().join(format!("tauminus-own-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Ours: accepted.
        let mine = dir.join("mine.sock");
        let _listener = tokio::net::UnixListener::bind(&mine).unwrap();
        assert!(crate::auth::own_socket(&mine).await.is_some());
        // Root's, open to all, as docker.sock is to its group: refused.
        let sudo = std::process::Command::new("sudo")
            .args(["-n", "true"])
            .status()
            .is_ok_and(|s| s.success());
        if !sudo {
            assert!(std::env::var_os("CI").is_none(), "CI runners have sudo");
            return;
        }
        let theirs = dir.join("root.sock");
        let script = format!(
            "import os,socket\ns=socket.socket(socket.AF_UNIX)\ns.bind({path:?})\n\
             os.chmod({path:?},0o777)\ns.listen(1)\ns.settimeout(10)\n\
             try:\n  s.accept()\nexcept Exception:\n  pass\nos.unlink({path:?})\n",
            path = theirs.display().to_string()
        );
        let mut root = std::process::Command::new("sudo")
            .args(["-n", "python3", "-c", &script])
            .spawn()
            .unwrap();
        for _ in 0..500 {
            if theirs.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(theirs.exists(), "root's socket is up");
        assert!(crate::auth::own_socket(&theirs).await.is_none());
        // It accepted once and is gone; nothing is left to kill.
        assert!(root.wait().unwrap().success());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slots_are_bounded_and_freed() {
        let forward = AgentForward::new(AgentSource::None);
        let slots: Vec<Slot> = (0..MAX_CHANNELS).map_while(|_| forward.reserve()).collect();
        assert_eq!(slots.len(), MAX_CHANNELS);
        assert!(forward.reserve().is_none());
        drop(slots);
        assert!(forward.reserve().is_some());
    }
}
