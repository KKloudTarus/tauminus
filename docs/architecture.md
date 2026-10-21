# Architecture

How the code is split, how a terminal session flows through it, and where new
code goes. Decisions behind this layout are in [ADR 0003](adr/0003-workspace-layout.md).

## Layout

```
Cargo.toml                 virtual workspace manifest
crates/
  tauminus-domain/         rules and types, no I/O, no runtime
  tauminus-app/            use cases (SessionManager, LocalShells) and ports
  tauminus-pty/            adapter: local shells over portable-pty
apps/desktop/
  src-tauri/               Tauri shell: composition root, IPC commands, wire types
  src/
    app/                   React root, global styles, design tokens
    features/terminal/     TerminalController, TerminalView, input queue, ack batching, renderer choice
    features/workspace/    tabs, split-pane layout tree, PaneTree and TabBar views
    features/keymap/       shortcut parsing, defaults per platform, user overrides
    app/                   AppShell (actions, theme, palette state), App, CommandPalette
    ipc/                   the only code that imports @tauri-apps; bindings.ts is generated
xtask/                     `cargo xtask ci`, `deps`, `bindings`
docs/                      roadmap, ADRs, benchmarks
```

## Dependency direction

```
tauminus-desktop ──► tauminus-pty ──► tauminus-app ──► tauminus-domain
        │                                  ▲
        └──────────────────────────────────┘
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
`deny.toml` bans `portable-pty` outside `tauminus-pty` and `tauri` and
`specta` outside the desktop shell.

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
 xterm.js ◄─write─ TerminalController ◄─ Channel<OutputChunk> (raw ArrayBuffer) ◄─ ChannelSink
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

### Complexity

| Operation | Time | Space |
|---|---|---|
| Registry lookup per write, resize, or ack | O(1) expected | O(s) for s sessions |
| Batching a chunk of c bytes | O(c); full chunks pass through uncopied, others are copied into the buffer and once more into the batch | O(F) per session |
| Flow control update | O(1) | O(1) |
| Input queue, n bytes in m chunks | O(n + m) | O(n) until sent |
| WebGL budget, acquire or release | O(1) | O(12) |
| Split, close, resize, or spatial focus in a tab | O(n), n ≤ 31 nodes | O(n) for the rebuilt path |
| Shortcut lookup per keystroke | O(1) | O(b) for b bindings |

### The view side

`TerminalController` lives outside React and owns the `Terminal` and its
session. `TerminalView` attaches it to a DOM node on mount and detaches it on
unmount; moving a pane remounts the view without touching the session.

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

### Adding a transport (SSH next)

1. Add a crate `crates/tauminus-ssh` that turns a `russh` channel into a
   `TerminalStream`: bounded input and event channels plus a `Terminator`
   that closes the channel.
2. Add a connector port in `tauminus-app::ports` and a use case next to
   `LocalShells` that reserves a slot, connects, and calls
   `SessionManager::attach`. The pump, batching, and flow control are reused
   as they are.
3. Ban `russh` outside `tauminus-ssh` in `deny.toml`.
4. Add a command in the desktop shell and regenerate bindings.

## Generated code

`apps/desktop/src/ipc/bindings.ts` comes from the Rust command signatures via
tauri-specta. `cargo test -p tauminus-desktop` regenerates it, and CI fails if
the committed copy differs.
