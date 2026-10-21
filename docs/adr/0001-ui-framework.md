# ADR 0001: React for the UI

- Status: accepted
- Date: 2026-10-21

## Context

Tauminus is a Tauri 2 desktop app. Terminal rendering uses `xterm.js` in the
webview. The UI needs tabs, split panes, a command palette, virtualized lists,
dialogs, and drag and drop. The project is open source and wants outside
contributors. The maintainer is learning Rust at the same time.

Options: Svelte 5, React 19, SolidJS.

Criteria and weights, fixed before evaluation: `xterm.js` integration and output
streaming (25), component ecosystem (20), contributor pool (20), maintainability
while learning Rust (15), bundle size and memory (10), official Tauri template
(10).

## Decision

React 19 with Vite and TypeScript.

## Basis

- The framework sits off the output path. Bytes go from Rust through
  `tauri::ipc::Channel` to `term.write(Uint8Array)`. `xterm.js` renders outside
  the component tree. The Tauri docs say the event system "is not designed for
  low latency or high throughput situations" and recommend channels for
  streaming. The streaming criterion is a tie.
- Contributor pool: weekly npm downloads on 2026-10-21 were `react` 116,163,376,
  `svelte` 4,210,714, `solid-js` 4,649,796.
- Ecosystem: split panes decide it. `react-resizable-panels` 4.14.8 was
  published 2026-10-20 with 18.8M weekly downloads. The only Svelte option,
  `paneforge` 1.0.2, was last pushed 2025-08-02.
- Templates: `create-tauri-app` 4.7.5 ships `react-ts`, `svelte-ts`, and
  `solid-ts`. Tie.
- Solid has `2.0.0-rc.16` on the `next` tag (2026-10-20). Starting on 1.9.17
  means a breaking migration soon.

## Consequences

- One `TerminalHost` component owns the `Terminal` instance and the channel.
  Terminal bytes never enter React state, context, or a store.
- Effects that create a `Terminal` or subscribe a channel must clean up
  (`term.dispose()`, unsubscribe), because StrictMode mounts effects twice in
  development.
- Larger runtime than Svelte or Solid. The app loads from local disk, so the
  cost is parse time and memory, not download time.

## Dissent

The Svelte case held that contributors to an SSH tool are mostly Rust
developers, who read `.svelte` files more easily than React hooks. No data
supported it, and it did not outweigh a 25x gap in framework familiarity. If the
maintainer knows Svelte well and React not at all, this decision should be
revisited.
