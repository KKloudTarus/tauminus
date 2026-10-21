# Contributing to Tauminus

Thanks for helping. This file covers the setup, the checks a pull request must
pass, and the rules that keep the codebase easy to change.

## Setup

Requirements:

- Rust: installed by `rustup` from `rust-toolchain.toml` (currently 1.99.1).
- Node.js 24 or later.
- pnpm 12: run `corepack enable`, or install pnpm 12.14.0 yourself.
- Platform libraries for Tauri: see https://v2.tauri.app/start/prerequisites/.
  On Debian and Ubuntu:

  ```sh
  sudo apt-get install libwebkit2gtk-4.1-dev build-essential curl wget file \
    libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
  ```

Then:

```sh
pnpm install
pnpm exec playwright install chromium   # browser tests
pnpm dev                                # run the app with hot reload
```

Optional git hooks run format and lint checks before each commit:
`pnpm hooks:install`. The hooks set `core.hooksPath`, so skip them if you
already manage hooks another way.

## Checks

`cargo xtask ci` runs the same checks as CI on your platform:

| Check | Command |
|---|---|
| Rust format | `cargo fmt --all --check` |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Rust tests | `cargo test --workspace` |
| IPC bindings are current | `cargo xtask bindings`, then commit `apps/desktop/src/ipc/bindings.ts` |
| Frontend format, lint, types, tests | `pnpm format:check`, `pnpm lint`, `pnpm typecheck`, `pnpm test` |

CI also runs `cargo deny`, `reuse lint`, `typos`, and `zizmor`.

## Rules

- **Tests ship with the change.** A behavior change without a test is not
  ready for review. Fakes of our own ports are fine; mocks of third-party
  types are not.
- **Layers point inward.** See [docs/architecture.md](docs/architecture.md).
  `tauminus-domain` depends on no runtime or driver. Adapters implement ports
  from `tauminus-app`. Only `apps/desktop/src-tauri` knows about Tauri.
  `deny.toml` enforces part of this in CI.
- **Terminal bytes never enter React state.** `TerminalView` writes them
  straight into xterm.js.
- **The frontend reaches the backend only through `src/ipc`.** Lint blocks
  `@tauri-apps/*` imports anywhere else.
- **New dependencies need a reason.** Say in the pull request what it replaces,
  its license, and its maintenance status. The license must pass `deny.toml`.
- **Every source file starts with an SPDX header:**

  ```rust
  // SPDX-FileCopyrightText: 2026 Tauminus contributors
  // SPDX-License-Identifier: GPL-3.0-or-later
  ```

## Commits and pull requests

Commit messages and pull request titles follow
[Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):
`feat(pty): resize on window change`. One logical change per commit. Each
commit builds and passes tests.

Open pull requests against `main`. Fill in the template, including how you
verified the change.

## License

Tauminus is GPL-3.0-or-later. By contributing you agree that your contribution
is licensed under the same terms.
