# ADR 0003: Workspace layout and project tooling

- Status: accepted
- Date: 2026-10-21

## Context

The skeleton fixes choices that are expensive to change later: crate
boundaries, how ports are expressed in Rust, how IPC types reach TypeScript,
and which tools gate a pull request. Generic clean-architecture advice and
current Rust and TypeScript practice disagree in a few places. Each choice
below was checked against official docs and against Tauri, Zed, GitButler,
Yaak, Cap, ruff, and uv as they stood on 2026-10-21.

## Decisions

### Crates by subsystem, not one crate per layer

`tauminus-domain` and `tauminus-app` are separate crates. Adapters split by
subsystem (`tauminus-pty`, later `tauminus-ssh` and `tauminus-store`), not
into a single `adapters` crate. GitButler, Yaak, and Zed split by subsystem;
none of them uses one crate per layer. Each extra crate costs link time and
manifest upkeep, so a crate exists only where a boundary needs enforcing.

### Ports as bounded channels

A transport is a pair of bounded `tokio::sync::mpsc` channels
(`TransportHandle`), not an async trait object. `async fn` in traits is not
object safe, and boxing every call to get `dyn` dispatch adds allocation on the
hottest path. Channels also carry backpressure for free. The launcher port is a
plain synchronous trait.

### Wire types live in the desktop shell

Domain types carry no serde or specta derives. `apps/desktop/src-tauri/src/ipc.rs`
defines DTOs and converts at the boundary. Yaak and Cap put derives on model
types instead; we accept a little duplication to keep the domain free of
framework types.

### Typed IPC with tauri-specta

`tauri-specta` 2.0.0-rc.25, pinned with `=`, generates `bindings.ts` from the
command signatures. It types commands and channels, where `ts-rs` covers types
only. The release candidate breaks between versions, so Dependabot ignores it
and it is upgraded by hand.

Terminal output uses a `Channel<OutputChunk>`. `OutputChunk` implements
`IpcResponse` as `InvokeResponseBody::Raw`, so JavaScript receives an
`ArrayBuffer`. A `Channel<Vec<u8>>` would serialize to a JSON number array.

### Frontend tooling

- pnpm 12 workspace, versions pinned exactly.
- TypeScript 7.0 for type checking. TS 7 ships no compiler API, so
  `typescript-eslint` cannot run; oxlint with type-aware rules (via
  `oxlint-tsgolint`) and oxfmt replace ESLint and Prettier, as in
  `create-vite` and GitButler.
- The React Compiler rules load from `eslint-plugin-react-hooks` as an oxlint
  JS plugin.
- Vitest 5 with two projects: Node for pure logic, Chromium through Playwright
  for components, because xterm.js needs real layout and canvas.

### Security defaults

- App commands get generated `allow-*` permissions from `build.rs`, and the
  main window's capability grants only the commands it calls.
- A strict CSP. `style-src` allows inline styles because xterm.js injects them.
- `freezePrototype` is off. `@xterm/addon-webgl` 0.19.0 bundles VS Code's
  `KeyCodeUtils`, which runs `e.toString = t` at module load. With
  `Object.prototype` frozen, that assignment hits JavaScript's "override
  mistake" (the inherited `toString` is non-writable), so WebKitGTK throws
  `TypeError: Attempted to assign to readonly property` and the bundle stops
  before React starts. Reproduced on 2026-10-21 by capturing the error in the
  running app. Re-enable once the addon stops assigning `toString`; the CSP
  already blocks the script injection that freezing would defend against.

### Terminal renderer

xterm.js renders with WebGL on WKWebView and WebView2, and with the DOM
renderer on WebKitGTK. On WebKitGTK the WebGL renderer left the screen blank
after `vim` switched to the alternate buffer until something forced a redraw;
the DOM renderer drew the same bytes correctly, which also rules out the IPC
path. The DOM renderer was about 15% slower and used less memory in the 100 MB
benchmark ([benchmarks](../benchmarks.md)). Loading a second WebGL addon into
a terminal also rendered nothing there, so a terminal that loses its context
or its slot in the WebGL budget stays on the DOM renderer.

### Repository checks

`cargo clippy` with `pedantic`, `cargo deny` (GPL-compatible license allowlist,
crate bans per layer), REUSE, typos, and zizmor for workflow security. Actions
are pinned by commit SHA. Git hooks (lefthook) are opt-in, because installing
them rewrites `core.hooksPath`.

## Rejected

- **One crate per layer** (`domain`, `application`, `adapters`): no surveyed
  project does it, and it puts unrelated drivers in one compilation unit.
- **`async-trait` ports:** boxed futures on every write and read.
- **ESLint with typescript-eslint:** blocked by TypeScript 7.
- **Mandatory pre-commit hooks:** conflict with contributors who already set
  `core.hooksPath`.
