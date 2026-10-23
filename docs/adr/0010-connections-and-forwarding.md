# ADR 0010: Shared SSH connections, port forwarding, and SFTP

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 adds SFTP and port forwarding. Both open channels on an SSH
connection, and so does a shell. Until now a shell owned its connection:
the bridge held every hop and disconnected them when the shell ended.
Opening SFTP for a host with a second login would ask for a password and a
one-time code again, and a forward would die with the tab that started it.

The workload is small. A user keeps a few connections open, tens at most.
A forward carries up to a few hundred TCP connections at a time (a browser
behind a SOCKS proxy opens about 50). SFTP must move 1 GB at the speed of
the link, which over 50 ms of round trip needs many reads in flight.

An architecture and a security review of the first draft shaped the
decisions below.

## Decisions

### A connection is shared through leases

`tauminus-ssh` keeps the hops of one login in a `Connection`. A shell, an
SFTP session, and each running forward hold an `Arc` to it. When the last
one drops, `Drop` spawns the innermost-first disconnect on the runtime the
connection was opened on, so there is one teardown path. A `watch` fires
when the session ends for any reason: the handler of every hop flips it
when russh drops it. Forwards and SFTP select on it and report the loss.

A shell still opens its own connection, as before, and never shares it:
two tabs to one host are two logins, and SFTP and forwards for a saved
host share a connection of their own, reused while it lives. The next
section says why a shell cannot be part of it.

The app layer keeps the registry in a `Connections` use case: saved host
id to a weak reference and the target it was opened for. A connection is
reused only while it is open and the host resolves to the same target and
identity. Resolving needs the vault open when the host uses an identity,
so a locked vault stops reuse as it stops a new login. SFTP and forwards
never ask for agent forwarding, so the agent stays reachable only through
the shell that asked for it. Ad hoc targets from the connect dialog are
never reused. Two acquires for one host at once share one login: the
second waits on a per-host lock and then finds the first one's
connection. A lookup is a hash map probe, O(1).

The port is a coarse trait, `SshConnection`: open a shell, start a
forward, open SFTP, and wait for the end. Byte streams and listeners stay
in the adapter. App code depends on tokio `sync` only.

### One channel never stalls the others

russh gives a channel's window back to the server when data arrives, not
when it is consumed, and its session loop waits while a channel's queue is
full. One reader that falls behind therefore paces every channel on the
connection, SFTP and keepalives included. Measured on this branch: a shell
printing `cat /dev/zero` at the speed its view renders slowed a forward on
the same connection about 70 times.

Two ways out were weighed by a panel. Patching russh so the window
follows consumption, as OpenSSH does, fixes the cause, but upstream closed
exactly that change (russh PR 730, "incorrect library use"), so it would
be a fork carried forever over a crate with 19 security advisories in six
months, and cargo-deny cannot check a vendored crate against them. The
choice is to keep every reader on a shared connection one that never
waits:

- Shells keep their own connections, where a slow view slows only itself.
- SFTP's client reads replies as they come and limits data by requests
  in flight, so it never stalls its connection.
- Every forwarded channel is read at once into a buffer of its own, at
  most 4 MiB, drawn from a budget of 64 MiB per connection. A peer that
  falls 4 MiB behind, or takes nothing for 30 s, is reset with a TCP RST,
  so it cannot take what it got for the whole stream. When the budget
  runs out, the channel holding the most of it goes, not the one that
  asked last, and each queued packet counts at least 128 bytes. One task
  per channel reads the server and writes the peer, whichever is ready,
  with vectored writes; the dialog counts the connections reset this way.

The costs: SFTP or a forward opened while only a shell is connected logs
in once more, which on a host with one-time codes means a second prompt;
and a local client that pauses a large download behind the SOCKS proxy
loses that connection, where OpenSSH would hold it. If russh ever grants
the window on consumption, both go away and shells can share again.

A remote rule connects to its destination before it accepts the channel,
so the server sends nothing while that connect is slow, and a stop ends a
SOCKS handshake or channel open still under way.

Handler callbacks never wait on I/O. Every channel kind the server may
open is rejected except agent channels for the shell that asked and
`forwarded-tcpip` for a running rule; russh's defaults accept them all.

### Forwarding

- Local (`-L`): a `TcpListener` on the bind address; each accepted socket
  opens a `direct-tcpip` channel (15 s to confirm) and the bytes are
  copied both ways. Loopback binds 127.0.0.1 and ::1, each IPv6 socket
  with `IPV6_V6ONLY`; the second is skipped only on a machine without
  IPv6. Anything else, such as another user already holding `[::1]` on
  that port where clients look first, fails the rule.
- Remote (`-R`): `tcpip-forward` on the server, then `cancel-tcpip-forward`
  on stop. The last hop's handler matches each `forwarded-tcpip` channel
  by the address and port the server reports (the port from its reply
  when the rule asked for 0) and hands it to the rule's task with
  `try_send`: no rule is `AdministrativelyProhibited`, a full queue is
  `ResourceShortage`. The task connects to the rule's destination, never
  to an address the channel names.
- Dynamic (`-D`): a SOCKS5 server on the bind address, `CONNECT` only,
  no authentication, IPv4, IPv6, and domain names; SOCKS4 and 4a too,
  since some tools still speak only those. Every field is read by its
  length or up to 255 bytes, names must be valid host names, and the whole
  handshake has 10 s. At most 64 handshakes run at once, and a connection
  slot is taken only after one succeeds, so clients that connect and say
  nothing cannot use up the slots.

Bind addresses are loopback (127.0.0.1 and ::1 here, `localhost` on the
server), every interface, or an IP literal; host names are not accepted,
so no resolver decides what is exposed. Anything but loopback opens the
tunnel to the network and the UI marks it. Each rule allows 256
connections at once, counted from accept; one more is accepted and closed
at once, so the client sees an error instead of hanging.

Saving a host stops any of its running rules that the edit removed or
changed, so nothing keeps listening the way the list no longer says.
Rules are saved with the host in `hosts.json`. A file with rules is
format version 2; one without stays version 1, so an older build keeps
reading it. A rule can start when the host connects. The palette lists
every rule with start and stop.

### SFTP

`russh-sftp` 3.0.1 (Apache-2.0) speaks the protocol over a channel
stream. Its `File` keeps reads and writes in flight (16 of each by
default, configurable), so a 50 ms link is not capped at one request per
round trip; transfers raise both to 64. A read reply longer than asked is an error; a short one is
asked again for the rest; only `SSH_FX_EOF` ends a file.

Names from the server are bytes it chose. Each one becomes a local path
component only after a check that refuses empty, `.`, `..`, separators,
NUL, control and bidirectional characters, and on Windows `:`, `*?"<>|`,
trailing dots and spaces, and device names. A listing keeps at most
100 000 entries.

A download writes to `<name>.tauminus-part`, created new (never through
an existing file or link), beside a record of the host, remote path, size,
and modification time, and renames it when complete. A resume continues
only when that record matches the remote file now and the last 64 KiB on
both sides agree; otherwise it starts over. An upload writes to a
temporary remote name and renames it over the target. The rules that
decide resume live in the domain, where they are tested without a
server. Transfers run one at a time per connection, four connections at
once.

Editing a remote file downloads it into a new private directory (0700),
opens it with the editor the user configured, or `$VISUAL`, `$EDITOR`,
`open -t`, or Notepad, as a program with the path as its last argument,
never through the file's type association. It uploads when the content's
hash changes, only to the connection it came from, and only if the
remote file has not changed since. The directory is removed when the edit
ends, and leftovers are removed at start.

## Consequences

- A forward and an SFTP pane survive closing any tab, and share one login
  per host between them.
- One connection carries several kinds of channel, so a broken one stops
  all of them at once. Each reports it, and the UI offers to reconnect.
- A loopback forward is open to every local user and process, as with
  OpenSSH. SOCKS authentication and Unix-socket listeners would close
  that and are left for later.
- Forwards keep running after the vault locks, as shells do.
