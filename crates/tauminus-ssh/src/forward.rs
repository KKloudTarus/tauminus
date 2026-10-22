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
            if let Err(error) = tokio::io::copy_bidirectional(&mut remote, &mut agent).await {
                tracing::debug!(%error, "agent forwarding channel ended");
            }
        });
    }
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
    let path = match source {
        AgentSource::Environment => std::path::PathBuf::from(std::env::var_os("SSH_AUTH_SOCK")?),
        AgentSource::Path(path) => path.clone(),
        AgentSource::Pageant | AgentSource::None => return None,
    };
    let stream = tokio::net::UnixStream::connect(path).await.ok()?;
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
