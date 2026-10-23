// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port forwarding on a [`Connection`]: local (`ssh -L`), remote
//! (`ssh -R`), and dynamic (`ssh -D`).
//!
//! Each rule runs on its own tasks and holds a lease on the connection.
//! Dropping the [`ForwardHandle`] closes the stop channel every task
//! watches, which ends the listener and every connection it carries.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client::{ChannelOpenHandle, Msg};
use russh::{Channel, ChannelOpenFailure};
use tauminus_app::ports::{ForwardHandle, ForwardStatus, TunnelError};
use tauminus_domain::{Bind, Destination, Forward};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tracing::debug;

use crate::connection::Connection;
use crate::pipe::pipe;
use crate::socks;

/// Connections one rule carries at once.
pub(crate) const MAX_OPEN: usize = 256;
/// How long the server may take to open a channel, and a local
/// destination to accept a connection.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a SOCKS client may take over its whole request.
const SOCKS_TIMEOUT: Duration = Duration::from_secs(10);
/// Channels the server may queue for a remote rule before it is refused.
const REMOTE_QUEUE: usize = 16;
/// SOCKS handshakes one rule reads at once. A slot for a connection is
/// taken only after its handshake, so clients that connect and say nothing
/// hold one of these for at most [`SOCKS_TIMEOUT`], not a connection slot.
const MAX_HANDSHAKES: usize = 64;

/// A `forwarded-tcpip` channel the server opened, waiting for its rule.
pub(crate) struct Incoming {
    channel: Channel<Msg>,
    reply: ChannelOpenHandle,
}

struct Route {
    id: u64,
    address: String,
    /// 0 until the server's reply names the port it picked.
    port: u32,
    sender: mpsc::Sender<Incoming>,
}

/// The remote rules running on a connection, for its last hop's handler.
/// At most a few dozen, so a list beats a map.
#[derive(Default)]
pub(crate) struct Routes {
    next: AtomicU64,
    entries: Mutex<Vec<Route>>,
}

impl std::fmt::Debug for Routes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Routes").finish_non_exhaustive()
    }
}

impl Routes {
    fn add(&self, address: String, port: u32, sender: mpsc::Sender<Incoming>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.lock().push(Route {
            id,
            address,
            port,
            sender,
        });
        id
    }

    fn set_port(&self, id: u64, port: u32) {
        if let Some(route) = self.lock().iter_mut().find(|r| r.id == id) {
            route.port = port;
        }
    }

    fn remove(&self, id: u64) {
        self.lock().retain(|r| r.id != id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Route>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands a channel to the rule listening on `address:port`, or refuses
    /// it. Never waits, since it runs inside russh's session loop. The
    /// address must match as requested, which OpenSSH echoes; a server that
    /// rewrites it still matches when only one rule has that port.
    pub(crate) fn dispatch(
        &self,
        address: &str,
        port: u32,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
    ) {
        let sender = {
            let routes = self.lock();
            let keys: Vec<(&str, u32)> = routes
                .iter()
                .map(|r| (r.address.as_str(), r.port))
                .collect();
            pick(&keys, address, port).map(|at| routes[at].sender.clone())
        };
        let Some(sender) = sender else {
            debug!(
                address,
                port, "refusing a forwarded channel no rule asked for"
            );
            return drop_reply(reply, ChannelOpenFailure::AdministrativelyProhibited);
        };
        match sender.try_send(Incoming { channel, reply }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(incoming)) => {
                drop_reply(incoming.reply, ChannelOpenFailure::ResourceShortage);
            }
            Err(mpsc::error::TrySendError::Closed(incoming)) => {
                drop_reply(
                    incoming.reply,
                    ChannelOpenFailure::AdministrativelyProhibited,
                );
            }
        }
    }
}

/// Which route, given as `(address, port)` with port 0 while its reply is
/// pending, takes a channel for `address:port`: an exact match first, then
/// a pending route for that address, then the one route with that port
/// when the server rewrote the address. O(r).
fn pick(routes: &[(&str, u32)], address: &str, port: u32) -> Option<usize> {
    let find = |matches: &dyn Fn(&(&str, u32)) -> bool| routes.iter().position(matches);
    find(&|&(a, p)| a == address && p == port)
        .or_else(|| find(&|&(a, p)| a == address && p == 0))
        .or_else(|| {
            let mut same = routes
                .iter()
                .enumerate()
                .filter(|(_, route)| route.1 == port);
            let first = same.next()?;
            same.next().is_none().then_some(first.0)
        })
}

/// Refuses a channel without waiting: russh's reply only queues a message.
fn drop_reply(reply: ChannelOpenHandle, reason: ChannelOpenFailure) {
    tokio::spawn(reply.reject(reason));
}

/// What every task of one rule shares.
#[derive(Clone)]
struct Rule {
    connection: Arc<Connection>,
    stop: watch::Receiver<()>,
    status: Arc<watch::Sender<ForwardStatus>>,
    slots: Arc<Semaphore>,
    handshakes: Arc<Semaphore>,
    /// Held by every task of the rule; the handle's `finished` closes
    /// when the last one ends, so the port is free.
    _finished: Arc<watch::Sender<()>>,
}

impl Rule {
    /// Counts a connection reset for falling behind, so the view can say why.
    fn ended_with(&self, error: &crate::pipe::PipeError) {
        debug!(%error, "forwarded connection ended");
        if error.reset_peer() {
            self.status.send_modify(|s| s.resets += 1);
        }
    }

    fn budget(&self) -> Arc<crate::pipe::Ledger> {
        Arc::clone(&self.connection.ledger)
    }

    /// A slot for one more connection, or `None` at [`MAX_OPEN`].
    fn take_slot(&self) -> Option<Slot> {
        let permit = Arc::clone(&self.slots).try_acquire_owned().ok()?;
        self.status.send_modify(|s| {
            s.open += 1;
            s.total += 1;
        });
        Some(Slot {
            _permit: permit,
            status: Arc::clone(&self.status),
        })
    }

    /// Resolves when the rule should stop: its handle dropped, or the
    /// connection ended, which is recorded.
    async fn ended(&self) {
        let mut stop = self.stop.clone();
        let mut closed = self.connection.closed_watch();
        tokio::select! {
            _ = stop.changed() => {}
            _ = closed.wait_for(|closed| *closed) => {
                self.status.send_modify(|s| {
                    s.ended.get_or_insert_with(|| "the connection closed".to_owned());
                });
            }
        }
    }
}

struct Slot {
    _permit: OwnedSemaphorePermit,
    status: Arc<watch::Sender<ForwardStatus>>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.status.send_modify(|s| s.open -= 1);
    }
}

pub(crate) async fn start(
    connection: Arc<Connection>,
    forward: &Forward,
) -> Result<ForwardHandle, TunnelError> {
    use tauminus_app::ports::SshConnection;
    if !connection.is_open() {
        return Err(TunnelError::Closed);
    }
    let (stop_tx, stop) = watch::channel(());
    let (status_tx, status) = watch::channel(ForwardStatus::default());
    let (finished_tx, finished) = watch::channel(());
    let rule = Rule {
        connection,
        stop,
        status: Arc::new(status_tx),
        slots: Arc::new(Semaphore::new(MAX_OPEN)),
        handshakes: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
        _finished: Arc::new(finished_tx),
    };
    let port = match forward {
        Forward::Local { bind, to } => {
            let to = to.clone();
            listen(&rule, *bind, move |rule, socket, peer| {
                tokio::spawn(local(rule, socket, peer, to.clone()));
            })
            .await?
        }
        Forward::Dynamic { bind } => {
            listen(&rule, *bind, |rule, socket, peer| {
                tokio::spawn(dynamic(rule, socket, peer));
            })
            .await?
        }
        Forward::Remote { bind, to } => remote(&rule, *bind, to.clone()).await?,
    };
    Ok(ForwardHandle::new(port, status, stop_tx, finished))
}

/// Tries for this long when a port is in use, which it is for a moment
/// after the same rule stopped there, and retries port 0 when its second
/// address collides.
const BIND_RETRIES: u32 = 20;
const BIND_RETRY_EVERY: Duration = Duration::from_millis(50);

async fn listen<F>(rule: &Rule, bind: Bind, serve: F) -> Result<u16, TunnelError>
where
    F: Fn(Rule, TcpStream, SocketAddr) + Clone + Send + 'static,
{
    let mut tries = 0;
    loop {
        match listen_once(rule, bind, serve.clone()) {
            Err(TunnelError::Bind { source, .. })
                if source.kind() == std::io::ErrorKind::AddrInUse && tries < BIND_RETRIES =>
            {
                tries += 1;
                tokio::time::sleep(BIND_RETRY_EVERY).await;
            }
            result => return result,
        }
    }
}

/// Binds every address of `bind` and runs an accept loop on each. Returns
/// the port. A second address is skipped only when this machine has no
/// IPv6; any other failure, such as someone else holding `[::1]:port`
/// where clients look first, fails the rule.
fn listen_once<F>(rule: &Rule, bind: Bind, serve: F) -> Result<u16, TunnelError>
where
    F: Fn(Rule, TcpStream, SocketAddr) + Clone + Send + 'static,
{
    let error = |address: SocketAddr| {
        move |source| TunnelError::Bind {
            address: address.to_string(),
            source,
        }
    };
    let mut addresses = bind.address.local_addresses().into_iter();
    let first = SocketAddr::new(addresses.next().expect("at least one address"), bind.port);
    let listener = bind_one(first).map_err(error(first))?;
    // Port 0 picks one; the other addresses then use the same.
    let port = listener.local_addr().map_err(error(first))?.port();
    let mut listeners = vec![listener];
    for ip in addresses {
        let address = SocketAddr::new(ip, port);
        match bind_one(address) {
            Ok(listener) => listeners.push(listener),
            Err(e) if no_ipv6(&e) => debug!(%address, error = %e, "no IPv6 here"),
            Err(e) => return Err(error(address)(e)),
        }
    }
    for listener in listeners {
        tokio::spawn(accept_loop(rule.clone(), listener, serve.clone()));
    }
    Ok(port)
}

/// A listener on exactly `address`: an IPv6 one never takes IPv4 as well,
/// so loopback's and every interface's two listeners do not collide.
fn bind_one(address: SocketAddr) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    if address.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    // As std and tokio do on Unix, so a restarted rule can bind again at
    // once. Windows reads this as letting others share the port, so not there.
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    #[cfg(windows)]
    exclusive(&socket)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(1024)?;
    TcpListener::from_std(socket.into())
}

/// `SO_EXCLUSIVEADDRUSE`, which socket2 has no setter for: without it,
/// another process of this user could bind the same port more narrowly
/// and take the connections meant for the forward.
#[cfg(windows)]
#[expect(unsafe_code, reason = "setsockopt has no safe wrapper for this option")]
fn exclusive(socket: &socket2::Socket) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        SO_EXCLUSIVEADDRUSE, SOCKET, SOCKET_ERROR, SOL_SOCKET, WSAGetLastError, setsockopt,
    };
    /// The size of the `i32` the option takes.
    const LEN: i32 = 4;
    let on: i32 = 1;
    let handle = SOCKET::try_from(socket.as_raw_socket())
        .map_err(|_| std::io::Error::other("socket handle out of range"))?;
    // SAFETY: the socket is open for the duration of the call, and the
    // value points at an i32 whose size is the length passed.
    let result = unsafe {
        setsockopt(
            handle,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&raw const on).cast(),
            LEN,
        )
    };
    if result == SOCKET_ERROR {
        // SAFETY: reads this thread's last Winsock error; no arguments.
        return Err(std::io::Error::from_raw_os_error(unsafe {
            WSAGetLastError()
        }));
    }
    Ok(())
}

/// The error a machine without IPv6 gives for an IPv6 socket or address.
fn no_ipv6(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
    ) {
        return true;
    }
    // EAFNOSUPPORT: Linux 97, macOS 47, Windows WSAEAFNOSUPPORT 10047.
    matches!(error.raw_os_error(), Some(97 | 47 | 10047))
}

async fn accept_loop<F>(rule: Rule, listener: TcpListener, serve: F)
where
    F: Fn(Rule, TcpStream, SocketAddr),
{
    loop {
        let accepted = tokio::select! {
            biased;
            () = rule.ended() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((socket, peer)) => serve(rule.clone(), socket, peer),
            Err(error) => {
                // Out of file descriptors, usually; let some close.
                debug!(%error, "accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

/// Opens a `direct-tcpip` channel to `host:port`, bounded by
/// [`OPEN_TIMEOUT`].
async fn open_channel(
    rule: &Rule,
    host: &str,
    port: u16,
    peer: SocketAddr,
) -> Option<Channel<Msg>> {
    let open = tokio::time::timeout(
        OPEN_TIMEOUT,
        rule.connection.last().channel_open_direct_tcpip(
            host,
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        ),
    );
    // A stop does not wait for the server to answer.
    let opened = tokio::select! {
        opened = open => opened,
        () = rule.ended() => return None,
    };
    match opened {
        Ok(Ok(channel)) => Some(channel),
        Ok(Err(error)) => {
            debug!(%error, host, port, "the server could not open the channel");
            None
        }
        Err(_) => {
            debug!(host, port, "the server did not open the channel in time");
            None
        }
    }
}

async fn local(rule: Rule, socket: TcpStream, peer: SocketAddr, to: Destination) {
    // At the limit the socket closes at once, so the client sees an error.
    let Some(_slot) = rule.take_slot() else {
        return;
    };
    let _ = socket.set_nodelay(true);
    let Some(channel) = open_channel(&rule, to.host.as_str(), to.port(), peer).await else {
        return;
    };
    if let Err(error) = pipe(socket, channel, rule.budget(), rule.stop.clone()).await {
        rule.ended_with(&error);
    }
}

async fn dynamic(rule: Rule, mut socket: TcpStream, peer: SocketAddr) {
    let Ok(handshake) = Arc::clone(&rule.handshakes).try_acquire_owned() else {
        return;
    };
    let _ = socket.set_nodelay(true);
    let handshake_done = tokio::time::timeout(SOCKS_TIMEOUT, socks::accept(&mut socket));
    let accepted = tokio::select! {
        accepted = handshake_done => accepted,
        () = rule.ended() => return,
    };
    let request = match accepted {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            debug!(%error, "refused a SOCKS request");
            return;
        }
        Err(_) => return,
    };
    drop(handshake);
    let Some(_slot) = rule.take_slot() else {
        let _ = socks::refuse(&mut socket, request.version, socks::Refusal::General).await;
        return;
    };
    let Some(channel) = open_channel(&rule, &request.host, request.port, peer).await else {
        let _ = socks::refuse(&mut socket, request.version, socks::Refusal::General).await;
        return;
    };
    if socks::grant(&mut socket, request.version).await.is_err() {
        return;
    }
    if let Err(error) = pipe(socket, channel, rule.budget(), rule.stop.clone()).await {
        rule.ended_with(&error);
    }
}

async fn remote(rule: &Rule, bind: Bind, to: Destination) -> Result<u16, TunnelError> {
    let address = bind.address.remote_spelling();
    let (sender, mut incoming) = mpsc::channel(REMOTE_QUEUE);
    let routes = Arc::clone(&rule.connection.routes);
    // Listed before asking, so a connection that arrives before the reply
    // still finds its rule.
    let id = routes.add(address.clone(), u32::from(bind.port), sender);
    // On its own task, so a reply that comes after we stopped waiting is
    // still seen, and its listener cancelled.
    let (answer_tx, answer) = tokio::sync::oneshot::channel();
    {
        let connection = Arc::clone(&rule.connection);
        let address = address.clone();
        tokio::spawn(async move {
            let asked = u32::from(bind.port);
            let result = connection
                .last()
                .tcpip_forward(address.clone(), asked)
                .await;
            if let Err(Ok(port)) = answer_tx.send(result) {
                let port = if port == 0 { asked } else { port };
                cancel(&connection, address, port).await;
            }
        });
    }
    let asked = tokio::time::timeout(OPEN_TIMEOUT, answer).await;
    let Ok(Ok(asked)) = asked else {
        routes.remove(id);
        return Err(TunnelError::Denied(format!(
            "{address}:{}: the server did not answer",
            bind.port
        )));
    };
    let port = match asked {
        // A server that picked a port must say which.
        Ok(0) if bind.port == 0 => {
            routes.remove(id);
            return Err(TunnelError::Denied(format!("{address}: no port given")));
        }
        Ok(0) => bind.port,
        Ok(port) => {
            let Ok(port) = u16::try_from(port) else {
                routes.remove(id);
                return Err(TunnelError::Denied(format!("{address}: port {port}")));
            };
            port
        }
        Err(error) => {
            routes.remove(id);
            debug!(%error, "tcpip-forward refused");
            return Err(TunnelError::Denied(format!(
                "{}:{}",
                if address.is_empty() { "*" } else { &address },
                bind.port
            )));
        }
    };
    routes.set_port(id, u32::from(port));
    let rule = rule.clone();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                biased;
                () = rule.ended() => break,
                next = incoming.recv() => next,
            };
            let Some(Incoming { channel, reply }) = next else {
                break;
            };
            match rule.take_slot() {
                Some(slot) => {
                    tokio::spawn(remote_one(rule.clone(), channel, reply, slot, to.clone()));
                }
                None => reply.reject(ChannelOpenFailure::ResourceShortage).await,
            }
        }
        routes.remove(id);
        cancel(&rule.connection, address, u32::from(port)).await;
    });
    Ok(port)
}

/// Asks the server to stop listening, giving up after [`OPEN_TIMEOUT`] so
/// a server that never answers cannot keep the connection held.
async fn cancel(connection: &Connection, address: String, port: u32) {
    let cancelled = connection.last().cancel_tcpip_forward(address, port);
    let _ = tokio::time::timeout(OPEN_TIMEOUT, cancelled).await;
}

/// Connects to the destination before accepting the channel, so the
/// server sends nothing while the connect is slow.
async fn remote_one(
    rule: Rule,
    channel: Channel<Msg>,
    reply: ChannelOpenHandle,
    _slot: Slot,
    to: Destination,
) {
    let connect = tokio::time::timeout(
        OPEN_TIMEOUT,
        TcpStream::connect((to.host.as_str(), to.port())),
    );
    let connected = tokio::select! {
        connected = connect => connected,
        () = rule.ended() => return,
    };
    let socket = match connected {
        Ok(Ok(socket)) => socket,
        Ok(Err(error)) => {
            debug!(%error, "the local destination refused");
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return;
        }
        Err(_) => {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return;
        }
    };
    reply.accept().await;
    let _ = socket.set_nodelay(true);
    if let Err(error) = pipe(socket, channel, rule.budget(), rule.stop.clone()).await {
        rule.ended_with(&error);
    }
}

#[cfg(test)]
mod tests {
    use super::pick;

    #[test]
    fn an_exact_route_wins_over_a_pending_one() {
        // A pending `localhost:0` listed before `localhost:9000`.
        let routes = [("localhost", 0), ("localhost", 9000)];
        assert_eq!(pick(&routes, "localhost", 9000), Some(1));
        assert_eq!(
            pick(&routes, "localhost", 40_000),
            Some(0),
            "the pending one"
        );
        let routes = [("localhost", 9000), ("localhost", 0)];
        assert_eq!(pick(&routes, "localhost", 9000), Some(0));
    }

    #[test]
    fn a_rewritten_address_matches_only_a_port_no_one_else_has() {
        let routes = [("localhost", 9000), ("", 9001)];
        assert_eq!(pick(&routes, "127.0.0.1", 9000), Some(0));
        let shared = [("localhost", 9000), ("10.0.0.1", 9000)];
        assert_eq!(pick(&shared, "127.0.0.1", 9000), None, "ambiguous");
        assert_eq!(pick(&routes, "localhost", 7), None);
        assert_eq!(pick(&[], "localhost", 7), None);
    }
}
