# Architecture

How the code is split, how a terminal session flows through it, and where new
code goes. Decisions behind this layout are in [ADR 0003](adr/0003-workspace-layout.md).

## Layout

```
Cargo.toml                 virtual workspace manifest
crates/
  tauminus-domain/         rules and types, no I/O, no runtime
  tauminus-app/            use cases (SessionManager, LocalShells) and ports
  tauminus-presence/       adapter: Touch ID, Windows Hello, polkit before the keychain unlocks
  tauminus-pty/            adapter: local shells over portable-pty
  tauminus-ssh/            adapter: SSH shells over russh, known_hosts, auth, proxies, ssh_config import
  tauminus-store/          adapter: saved hosts in one JSON file, written atomically
apps/desktop/
  src-tauri/               Tauri shell: composition root, IPC commands, wire types
  src/
    app/                   React root, global styles, design tokens
    features/terminal/     TerminalController, TerminalView, input queue, ack batching, renderer choice
    features/workspace/    tabs, split-pane layout tree, PaneTree and TabBar views
    features/keymap/       shortcut parsing, defaults per platform, user overrides
    features/ssh/          quick connect, destination parsing, host key and secret prompts
    features/hosts/        saved hosts sidebar, search, host and group editors
    app/                   AppShell (actions, theme, palette state), App, CommandPalette
    ipc/                   the only code that imports @tauri-apps; bindings.ts is generated
xtask/                     `cargo xtask ci`, `deps`, `bindings`
docs/                      roadmap, ADRs, benchmarks
```

## Dependency direction

```
tauminus-desktop ──► tauminus-pty ──────► tauminus-app ──► tauminus-domain
        │      ├───► tauminus-ssh ──────►      ▲
        │      └───► tauminus-presence ─►      │
        └──────────────────────────────────────┘
```

- `tauminus-domain` depends on `thiserror` only. No tokio, no serde.
- `tauminus-app` depends on the domain and on tokio for channels, timers, and
  task spawning.
- Adapters depend on `tauminus-app` to implement its ports. They never depend
  on each other.
- `tauminus-desktop` wires adapters into use cases and is the only crate that
  knows about Tauri or specta.

The compiler enforces the direction, because a crate can only use what its
manifest lists. Two checks keep the manifests honest: `cargo xtask deps`
allowlists the normal dependencies of the domain and app crates, and
`deny.toml` bans `portable-pty` outside `tauminus-pty`, `russh` outside
`tauminus-ssh`, and `tauri` and `specta` outside the desktop shell. The
domain keeps serde out too: `tauminus-store` maps hosts to its file format.

## A terminal session, end to end

```
 xterm.js ─onData/onBinary─► InputQueue (one call in flight) ─► session_write ─► SessionManager.write
                                                                                   │ bounded mpsc (64)
                                                                                   ▼
                                                                            pty-writer thread ─► PTY ─► shell
 shell ─► PTY ─► pty-reader thread ─bounded mpsc (4 × 64 KiB)─► Pump task
                                                                   │ drains queued reads, then:
                                                                   │   quiet ≥ 8 ms → send now
                                                                   │   else → send at 8 ms or 64 KiB
                                                                   ▼
 xterm.js ◄─write─ OutputScheduler (one pane per turn) ◄─ TerminalController ◄─ Channel<OutputChunk> (raw ArrayBuffer) ◄─ ChannelSink
     │
     └─ write callback ─► AckBatcher (64 KiB) ─► session_acknowledge ─► FlowControl
```

`LocalShells::open` reserves a session slot, launches the shell, and attaches
the resulting `TerminalStream` to the `SessionManager`. The manager knows
nothing about how a stream started, so SSH reuses it unchanged.

### Flush policy

A send that comes at least 8 ms after the previous one goes out at once, so a
keystroke echo pays no batching delay. Otherwise output is held until 8 ms
after the previous send or until 64 KiB accumulate. For a session producing r
bytes per second, the IPC rate is therefore at most 125 + r / 64 KiB messages
per second, because a full batch goes out without waiting and restarts the
interval: about 125 for a busy prompt, about 600 at 30 MB/s. Without
batching it would be one message per PTY read, up to about 200,000 per second
for `yes`.

### Backpressure

The view acknowledges bytes after xterm.js has parsed them. `FlowControl`
tracks `f`, the bytes sent but not yet acknowledged, with watermarks
H = 256 KiB and L = 96 KiB:

1. When `f > H`, the pump stops reading its event channel.
2. The bounded channel fills, and the reader thread blocks on `blocking_send`.
3. The reader stops draining the PTY, the kernel buffer fills, and the shell
   blocks on its write.
4. When `f < L`, the pump resumes.

The view acknowledges in batches of k = 64 KiB, with at most one ack call in
flight; a failed call is retried, never dropped. Once the view has parsed
everything, at most k − 1 bytes remain unacknowledged, and k − 1 < L, so a
paused session always resumes. Tests pin H, L, and k on both sides.

Bounds, with F = 64 KiB the flush size and C = 64 KiB the largest PTY read:

- A batch is at most max(F, C) bytes, and the batcher holds at most F.
- Pausing is checked after each delivered batch, and one push can release two
  batches, so bytes in flight stay at most H + F + C = 384 KiB. A test drives
  this bound with small numbers.
- Memory per session, worst case: event queue 4 × C, the chunk a blocked
  reader holds (C), the reader's buffer (C), the batcher (F), and bytes in
  flight (H + F + C). That is 832 KiB, or 52 MiB for 64 sessions.

### Ending a session

`close` signals the pump, which calls the stream's `Terminator` directly
rather than queuing a message: a writer blocked on a full PTY input buffer
cannot delay it.

1. `terminate` sends SIGHUP to the terminal's foreground process group and to
   the shell's group, as closing a terminal window does.
2. The closing pump keeps draining its stream, so the reader can never block
   forever. Output that arrives while the view is behind is discarded rather
   than delivered.
3. After a 2 s grace period the pump calls `kill` (SIGKILL to the same groups)
   and releases the session slot.

A dedicated waiter thread reports the exit as soon as the shell is reaped,
without waiting for EOF, so a `nohup` job holding the PTY does not keep a
finished session open. Signals are sent only while a lock shows the shell has
not been reaped, and reaping happens only under that lock, so a pid that the
kernel has released and reused is never signalled. Dropping a terminator
terminates as well, so no shell outlives its session. A webview reload closes
every session.

### Concurrency

| Shared state | Protected by |
|---|---|
| Session registry (`HashMap<SessionId, SessionEntry>`) | `std::sync::Mutex`, never held across `.await` or a process launch |
| Session count | `tokio::sync::Semaphore`; the permit lives in each pump task |
| Acknowledged bytes, close signal (`SessionControl`) | `AtomicUsize` and two `Notify`s, read by the pump |
| Batcher and flow control state | Owned by one pump task, no lock |
| PTY reader, writer, and waiter | One OS thread each, communicating by channel |
| Shell reaped or alive (`ProcessGuard`) | `Mutex<bool>`, held while signalling and while reaping |
| PTY master | `Mutex`, shared by the writer (resize) and the terminator (foreground group) |

### Workspace

`Workspace` holds the tabs and, per tab, an immutable split-pane tree
(`layout.ts`) with these invariants: a split has at least two children; its
sizes are positive and sum to 100; no split repeats its parent's direction
(such children are flattened); ids are unique. A seeded property test applies
2,000 random sequences of splits, closes, and resizes and checks every
invariant after each step. Spatial focus (`neighbor`) maps the tree to unit
rectangles and picks the pane beyond the current edge with the smallest gap,
then the largest overlap.

Limits: 16 panes per tab, 64 panes in total to match the backend's session
limit. A pane whose shell exits with code 0 closes itself; any other exit
stays open so its message can be read.

App shortcuts avoid keys the shell needs: Ctrl+Shift on Linux and Windows,
Cmd on macOS. Plain Ctrl+K stays with the shell (kill to end of line), so the
palette is Ctrl+Shift+P, or Cmd+K on macOS. Chords match on
`KeyboardEvent.code`, so an input method such as Telex cannot move them.

### SSH sessions

An SSH session enters the same pump as a local shell: `SshSessions` reserves
a slot, `RusshConnector` returns a `TerminalStream`, and `SessionManager`
attaches it. Batching, flow control, and close escalation are shared.

A connect walks the hops in order. The first goes over the target's route
(direct TCP, `ProxyCommand`, SOCKS5, HTTP CONNECT); each later hop runs over a
`direct-tcpip` channel of the one before, as `ProxyJump` does. Every hop
checks its own host key and authenticates on its own.

Host keys follow OpenSSH, with the rules in
[ADR 0004](adr/0004-ssh-host-keys-and-auth.md):

- Any recorded key that does not match, whatever its type, stops the connect.
  The client offers recorded key types first, so a server that still has its
  recorded key presents it.
- `@revoked` stops the connect, for a plain key, a certificate's key, or its
  authority. A certificate that fails for any other reason falls back to its
  plain key.
- Only a host with nothing on record gets a prompt. Tauminus reads the user's
  and the system's `known_hosts` files and writes only its own, hashed, mode
  0600.

Key files are offered by their public half. The server asks for a signature
only for a key it accepts, so a passphrase is asked only then. Decryption runs
on the blocking pool: bcrypt takes about 200 ms per attempt.

The bridge reads and writes on separate tasks. A write waits while the
server's window is full, and the window reopens only after russh reads what
came before. One task doing both deadlocks after a paste larger than the
window (2 MiB with OpenSSH) into a program that echoes it.

Bounds per SSH session: 1 output chunk waiting in the bridge, 4 in the queue
to the pump, and 32 packets of at most 32 KiB in russh, so at most 1.2 MiB
while the view is paused, on top of the pump's 832 KiB. Prompts are bounded
too: 3 passphrase tries per key, 3 passwords, 10 keyboard-interactive rounds,
4 passes over the method list after partial successes.

### Session options

A host or group may forward the agent (`ssh -A`) and send environment
variables (`SetEnv`); importing `~/.ssh/config` reads both. Forwarding is the
nearest level's choice; variables add up from the root group down, and a
nearer level's value for a name wins. Both apply to the last hop only.
Variables go out without asking for a reply, as OpenSSH sends them, so a
name the server's `AcceptEnv` refuses is dropped, not an error.

The agent request goes out only for a host that forwards it, and the client
refuses any agent channel a server opens otherwise. Each accepted channel is
relayed to the local agent on its own task, with at most 16 open per
connection. The agent signs; no key leaves the machine. Anyone with root on
the server can use those signatures while connected, and the host editor
says so.

### Vault

`vault.json` holds identities (a user, a password, a private key, its
passphrase) encrypted with XChaCha20-Poly1305 under one random data key.
The master password (Argon2id) and, if the user turns it on, a key in the OS
keychain each wrap that data key. A host or group names an identity; the
nearest one wins. `host_connect` asks the vault for its credentials and
fails with `vault_locked` while it is locked. The identity's key and
password are tried on the last hop only, before any prompt. Secrets go into
the backend and never come back to the view. Details in
[ADR 0007](adr/0007-vault.md).

Unlocking with the keychain can ask the OS to confirm the user is present
first: Touch ID on macOS, Windows Hello, polkit on Linux, each behind the
`Presence` port in `tauminus-presence`. The backend runs the check, and the
keychain slot's kind, bound into its additional data, says whether it asks.
It guards the app's own unlock, not the keychain entry. Details in
[ADR 0009](adr/0009-presence-unlock.md).

An identity's key can be pasted (import) or generated in the backend:
Ed25519 by default, ECDSA P-256/384/521, or RSA 3072/4096. A generated key
is stored without a passphrase, since the vault encrypts it. "Install on
host" adds the public key to `~/.ssh/authorized_keys` on a saved host, as
`ssh-copy-id` does: the client signs in with that host's own settings and
prompts, opens an exec channel running `sh -s`, and sends a script on
stdin, so nothing is quoted twice. The key line is checked to hold nothing
a shell reads inside single quotes. The script creates `~/.ssh` (0700) and
the file (0600) when missing and adds the line once.

Costs: unlocking runs Argon2id once (64 MiB, about 0.3 s) off the async
workers. Listing, saving, and connecting decrypt the identity records, O(n)
for n ≤ 1000 identities, and a save rewrites the file.

### History

The view records each command it sees run, with its output, through
`history_record`; the backend drops it when the host records no history.
With the vault open, entries go into `history.db` (SQLite with FTS5,
encrypted by SQLCipher). With it locked, each is sealed to the history's
X25519 public key and appended to `history.pending`; the next unlock
imports them. Searching needs the vault open. Details in
[ADR 0008](adr/0008-history.md).

Costs: a search is an FTS5 match read newest first, so it stops at the
result limit: 9 ms at the 95th percentile over a million lines. Recording
one command is one insert, or one seal and fsync while locked. Pruning is
O(n) and runs on unlock and every 500 commands.

### Saved hosts

A host sets what it needs and inherits the rest from its group, then from
that group's parents: the nearest value wins. Jump hosts and the route
(ProxyCommand or a proxy) are one choice, so both come from the nearest
level that sets either. `Inventory` keeps four invariants on every write:
unique ids, existing groups, no cycles, depth at most 16. Deleting a group
moves its hosts and subgroups up a level with the settings they inherited,
so no host changes how it connects. Details in
[ADR 0005](adr/0005-saved-hosts.md).

`HostBook` applies each change to a copy, saves it, and only then shows it.
The store writes a temporary file, flushes it, and renames it over the old
one, so a crash leaves the old list or the new one. Before writing it
checks that the file still holds what it last read; if another window
changed it, the save is refused and the list reloaded.

Importing `~/.ssh/config` lists the `Host` aliases (with `Include`, which
OpenSSH applies conditionally) and resolves each one with `ssh -G`, so the
result is what `ssh` itself would use. Without an `ssh` binary a built-in
resolver handles the keywords Tauminus imports; a corpus of 33 configs
checks it against `ssh -G`. Reading is capped at 256 files and 8 MiB; each
`ssh -G` at 5 s, the whole import at 30 s.

A ProxyCommand reaches a shell, so only the store and the importer write
one. `ssh_open` refuses one from the view, and saving a host may keep a
stored ProxyCommand but not add or change it; saved hosts connect through
`host_connect`, which reads the target from the store.

### Prod guard

On a guarded host, `CommandGuard` in the view holds back Enter, reads the
line from the screen (from a marker set when the user started typing, and
the whole wrapped line), and sends both to `guard_check`. The backend reads
the host's policy from `HostBook` and runs `Policy::check` in the domain: a
lexer that never fails splits the line into every simple command it may
run, and the rules check each. Findings come back to a dialog over the
pane; Enter goes on only if the user picks Run. Later input waits behind
the check, so keystrokes stay in order. Details in
[ADR 0006](adr/0006-prod-guard.md) and [the guide](prod-guard.md).

### Complexity

| Operation | Time | Space |
|---|---|---|
| Registry lookup per write, resize, or ack | O(1) expected | O(s) for s sessions |
| Guard check of a line of n characters, w words | O(n·d + w·64) for nesting d ≤ 8, plus O(n·m) per pattern of m characters | O(n) |
| Batching a chunk of c bytes | O(c); full chunks pass through uncopied, others are copied into the buffer and once more into the batch | O(F) per session |
| Flow control update | O(1) | O(1) |
| Input queue, n bytes in m chunks | O(n + m) | O(n) until sent |
| WebGL budget, acquire or release | O(1) | O(12) |
| Output scheduler: push n bytes, send a chunk, grant or release a turn | O(n / 1 KiB) views, no copy; O(1) amortized per chunk; O(1) per turn | O(p) for p panes, plus what flow control lets each hold |
| Split, close, resize, or spatial focus in a tab | O(n), n ≤ 31 nodes | O(n) for the rebuilt path |
| Shortcut lookup per keystroke | O(1) | O(b) for b bindings |
| Load `known_hosts`, n bytes over all files | O(n), off the async workers | O(n) |
| Check a host key, L lines of k patterns | O(L·k·\|p\|·\|h\|) worst case, linear for plain patterns; one HMAC per hashed line; keys decoded only for matching lines | O(1) beyond the loaded file |
| Glob match of pattern p against host h | O(\|p\|·\|h\|), \|h\| ≤ 261 | O(1) |
| Prompt routing (register, reply, cancel) | O(1) expected | O(c) for c connects waiting |
| Resolve a saved host | O(d), d ≤ 16 | O(d) |
| Move a group | O(G·d) to recheck its subtree | O(1) |
| Save the host list | O(H + G): copy, serialize, one read for the conflict check, write; 44 ms at H = 10 000 | O(H + G) |
| Search saved hosts, T words | O(H·T·L), most fields rejected in O(1) by a letter mask; under 60 ms at H = 10 000 with every field at its limit | O(H) cached haystacks |
| Read ssh_config | O(n) in bytes read, at most 256 files and 8 MiB | O(n) |

### The view side

`TerminalController` lives outside React and owns the `Terminal` and its
session. `TerminalView` attaches it to a DOM node on mount and detaches it on
unmount; moving a pane remounts the view without touching the session.

Every pane parses its output on the one main thread. xterm.js parses for up
to 12 ms before it yields, so three panes flooding at once ran back to back,
dropped 41% of frames and delayed a keystroke echo by up to 91 ms.
`OutputScheduler` gives one pane at a time a turn of about 6 ms. A turn keeps
two 1 KiB chunks with xterm.js and sends the next from each chunk's parse
callback, so chunks within a turn cost no timer hop, and it ends on time,
not on bytes: `yes` parses far slower per byte than long lines. Turns go
round robin and pass on through a `MessageChannel` task, which the browser
never clamps the way it clamps nested timers. A write of at most 4 KiB to a
pane with nothing queued skips the line, so typing in a quiet pane stays
fast, and so does every write while the window is hidden.

A turn never waits on xterm.js for good: after 250 ms without a parsed
chunk it ends, and that pane gets no new turn until its chunk is back.
xterm.js 6 stops parsing for good after a throw inside its parser, and
without this rule one such pane would stop every pane. The REP sequence
(`CSI n b`) was one such throw: xterm.js allocates n cells at once, so
`CSI 2147483647 b` asks for 8 GiB. The terminal drops a REP longer than one
screenful. [benchmarks.md](benchmarks.md) has the numbers before and after.

Browsers cap live WebGL contexts at about 16 and silently drop the oldest.
`WebglBudget` hands out 12 slots and evicts the least recently attached
terminal instead. A terminal that loses its slot or its context moves to the
DOM renderer for good, because loading a second WebGL addon into the same
terminal renders nothing on WebKitGTK.

## Where new code goes

| Kind of code | Place |
|---|---|
| A rule that holds for any transport (size limits, flow control) | `tauminus-domain` |
| A use case (open, write, close a session) | `tauminus-app` |
| A port a use case needs (launcher, store, clock) | `tauminus-app/src/ports.rs` |
| Talking to the OS, network, or a library | a new or existing adapter crate under `crates/` |
| An IPC command or wire type | `apps/desktop/src-tauri/src/{commands,ipc}.rs` |
| A UI feature | `apps/desktop/src/features/<feature>/` |
| A backend call from the UI | `apps/desktop/src/ipc/` |

### Adding a transport

SSH went in this way; serial or Mosh would follow the same steps.

1. An adapter crate turns the transport into a `TerminalStream`: bounded
   input and event channels plus a `Terminator`.
2. A connector port in `tauminus-app::ports` and a use case next to
   `LocalShells` that reserves a slot, connects, and calls
   `SessionManager::attach`. The pump, batching, and flow control are reused
   as they are.
3. A `deny.toml` ban keeps the transport library inside its crate.
4. A command in the desktop shell, then regenerated bindings.

## Generated code

`apps/desktop/src/ipc/bindings.ts` comes from the Rust command signatures via
tauri-specta. `cargo test -p tauminus-desktop` regenerates it, and CI fails if
the committed copy differs.
