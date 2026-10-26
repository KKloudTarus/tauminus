# Roadmap

Each version has a goal, a scope checklist, and exit criteria. A version ships
when every exit criterion passes on Linux, macOS, and Windows in CI, unless the
item names a platform. Feature sources and library choices are in
[feature-parity.md](feature-parity.md).

Rules that apply to every version:

- Every behavior change ships with tests in the same pull request.
- Terminal bytes never enter React state. A `TerminalController`, outside
  React, owns each `xterm.js` instance and its session; views only attach and
  detach ([ADR 0001](adr/0001-ui-framework.md), [architecture](architecture.md)).
- The domain crate imports no Tauri, `russh`, `portable-pty`, or SQLite type.
- The app works fully without the Hub. Hub features are additive
  ([ADR 0002](adr/0002-hub-server.md)).

## v0.1: Daily driver

Goal: replace the system terminal plus `ssh` for one person on one machine.

### Scope

Project skeleton

- [x] Cargo workspace: `tauminus-domain`, `tauminus-app` (use cases and ports), `tauminus-pty`, `apps/desktop/src-tauri`; `tauminus-ssh` and `tauminus-store` follow
- [x] React 19 + Vite + TypeScript frontend
- [x] SPDX header `GPL-3.0-or-later` on every source file, `license` field in `Cargo.toml` and `package.json`
- [x] CI on `ubuntu-22.04`, `macos-latest`, `windows-latest`: build, `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`, `tsc --noEmit`, frontend tests
- [x] Terminal stream port in `tauminus-app`: bounded input and event channels plus a `Terminator`

Terminal

- [x] Local PTY adapter over `portable-pty` (bash, zsh, fish, PowerShell)
- [x] `TerminalController` with `@xterm/xterm` 6, addons `webgl`, `fit`, `search`, `unicode11`
- [x] Clickable links through a vetted opener (http and https only), opened with Ctrl+click (Cmd+click on macOS)
- [x] Output path: Rust coalesces output (immediate after a quiet period, otherwise every 8 ms or at 64 KiB) and sends raw bytes through `tauri::ipc::Channel`
- [x] Backpressure: pause reading above 256 KiB unacknowledged, resume below 96 KiB
- [x] Tabs, split panes, close and reorder (drag and drop, shortcuts)
- [x] Command palette (`Ctrl+Shift+P`; `Cmd+K` on macOS, since plain `Ctrl+K` belongs to the shell)
- [x] Themes shared by UI and terminal through CSS variables: system, light, dark
- [x] Keymap layer with defaults per platform and validated user overrides
- [x] Vietnamese Telex and VNI input through the OS IME (Telex tested live with ibus-unikey on WebKitGTK; macOS and Windows input methods not yet tried by hand)

SSH

- [x] SSH transport over `russh` 0.64 with the `ring` backend
- [x] Auth: password, keyboard-interactive, Ed25519/ECDSA/RSA-SHA2 keys with passphrase (asked only for a key the server accepts), OpenSSH user certificates; restarts after partial success
- [x] System ssh-agent: Unix socket and the Windows OpenSSH named pipe
- [x] Pageant (0.75 and later, through its per-user pipe; the pipe's owner is checked)
- [x] ProxyJump chains up to 8 hops, each with its own host key check
- [x] ProxyCommand
- [x] SOCKS5 and HTTP CONNECT proxy, without proxy authentication
- [x] `known_hosts` parser: hashed entries, wildcards, `!` negation, `@cert-authority`, `@revoked`; first-connect prompt; changed-key block screen
- [x] Readable errors with a next step for: auth failure per method, host key changed, DNS failure, connection refused, timeout
- [x] Quick connect (`Ctrl+Shift+O`, `Cmd+Shift+O` on macOS) with `ssh -J` style jump hosts; splitting an SSH pane opens the same host

Hosts

- [x] Host, group, and tag model with inherited settings
- [x] Host list sidebar with search (`Ctrl+Shift+H` shows or hides it)
- [x] Import `~/.ssh/config`: resolve with `ssh -G` when OpenSSH is present, else a built-in resolver checked against `ssh -G`
- [x] First-run screen offering the import

Release

- [x] `tauri-action` builds `.AppImage`, `.deb`, `.rpm`, `.dmg` (universal), `.msi`, `.exe` on tag push, unsigned
- [x] README section on opening an unsigned build on macOS and Windows

### Exit criteria

- [x] `vim`, `htop`, `tmux`, `less`, and `mc` render and respond correctly over local and SSH sessions (checked by hand on WebKitGTK, including mouse clicks in `mc` and resizing a split)
- [x] `cat` of a 500 MB file and `yes` for 60 s with 4 panes open: no dropped frames above 5 percent, UI input latency under 50 ms, webview RSS stable within 50 MB after the run. Measured on all three webviews and recorded in `docs/benchmarks.md` (WebKitGTK, WKWebView and WebView2 on GitHub runners, latency read at p99; see docs/benchmarks.md)
- [x] Idle app with one local tab: RSS and cold start time recorded next to Termius on the same machine (Linux and macOS runners; Termius's Windows download returned 404)
- [x] Integration tests run `russh` against OpenSSH in CI for every auth method and ProxyJump (key files, encrypted keys, user certificates, agent, password and keyboard-interactive through PAM, ProxyJump and ProxyCommand)
- [x] `known_hosts` parser passes a corpus of real-world files, including hashed and `@cert-authority` lines (GitHub's published keys, a file hashed by `ssh-keygen -H`, and a hand-edited file, each also checked against `ssh-keygen -F`)
- [x] `~/.ssh/config` import matches `ssh -G` output for a corpus of at least 20 configs (33 today)

## v0.2: Prod guard and memory

Goal: the reason to switch from Termius.

### Scope

Prod guard

- [x] `prod` tag: red frame, tab label, status bar color
- [x] Shell command parser that reads partial input (a hand-written lexer; see [ADR 0006](adr/0006-prod-guard.md))
- [x] Default rules: `rm -rf`, `DROP`/`TRUNCATE`, `kubectl delete`, `terraform destroy`, `shutdown`, `reboot`, `mkfs`, `dd of=/dev/`, `> /dev/sd*`, `chmod -R 777 /`
- [x] Rules editable per host and group, with allow and deny lists
- [x] Confirmation dialog shows the exact command and host name; default button is cancel
- [x] Multi-line paste into a `prod` host needs confirmation

History

- [x] Record commands and output per session in SQLite with FTS5, encrypted at rest
- [x] One search box across all hosts; filters by host, time, exit code
- [x] Recording off per host; no capture while the remote echo is off (password prompts)
- [x] Size cap and retention policy with automatic cleanup

Vault

- [x] Vault file: Argon2id key derivation, ChaCha20-Poly1305 per record
- [x] Master password unlock, OS keychain unlock, auto-lock after idle time
- [x] Biometric unlock: Touch ID (macOS), Windows Hello; Linux falls back to keychain plus polkit (ADR 0009)
  - [x] Linux: polkit run against a real polkitd and agent
  - [x] macOS and Windows: the OS calls run in CI on both runners, which have no sensor
  - [x] Accepted on 2026-10-22 on the evidence above, by the maintainer: a Touch ID or Windows Hello prompt answered on real hardware is still to be run, with [these steps](testing/presence.md), when such a machine is at hand
- [x] Identities: reusable user, password, and key bundles
- [x] Key generation (Ed25519, ECDSA, RSA), import, export public key to a server

Session

- [x] Agent forwarding per host
- [x] Environment variables per host and group

### Exit criteria

- [x] Prod guard rule tests: at least 200 commands (dangerous and safe), zero false negatives on the dangerous set, false positive rate recorded
- [x] Wrapped and obfuscated forms caught: `sudo`, `env`, `bash -c`, `;`, `&&`, `|`, `$(...)`, backticks
- [x] Vault tests: round trip, wrong password, tampered ciphertext, truncated file
- [x] Secrets never appear in logs, crash reports, or history (test scans the store and log files)
- [x] History search over 1 million recorded lines returns in under 100 ms (benchmark in CI)
- [x] Security review of vault and history storage ([record](security-review-v0.2.md))

## v0.3: Files, tunnels, and more protocols

Goal: Termius Starter parity on desktop.

### Scope

- [x] SFTP dual pane, drag and drop from the OS, edit remote file with re-upload, chmod, resume on reconnect
- [x] Local, remote, and dynamic forwarding; saved rules; start and stop from the palette
- [x] Snippets with folders and `{{variable}}` placeholders; startup snippets per host
- [x] Run a snippet on many hosts; broadcast input to selected panes with instant Ctrl+C
- [x] Workspaces: save and restore layouts with their hosts
- [x] Session status dots: unread output, waiting for input, error
- [x] Auto-reconnect after sleep or network change, reattach `tmux` when the host uses it
- [x] Telnet transport (RFC 854, NAWS, TTYPE, ECHO)
- [x] Serial transport with port enumeration, baud and framing settings
- [x] Mosh on macOS and Linux through the upstream `mosh-client`
- [x] Character encoding per host

### Exit criteria

- [x] SFTP: 1 GB upload and download checksum match; interrupted transfer resumes
- [x] Forwarding tests in CI for all three types
- [x] Reconnect test: kill the network for 30 s, session resumes and `tmux` reattaches
- [x] Telnet test against a local `telnetd`; serial test against a virtual port pair

## v0.4: Recording, automation, hardware keys

Goal: beat Termius Pro.

### Scope

- [x] Session recording to asciicast v2, replay player with seek and search
- [x] Bookmarks and comments on recordings
- [x] Triggers: regex match runs highlight, notify, send text, or capture
- [x] Highlight sets per host and group
- [x] Autocomplete from forked Fig specs for bash and zsh hosts whose shell marks its prompt (OSC 133)
- [x] Password suggestion at detected prompts from identities
- [x] AI assistant, off by default: text to command, explain error; user key or local model; every command passes the prod guard
- [x] `tmux` control mode: `tmux` windows as native tabs (tmux 3.2+, one pane per window)
- [x] FIDO2 `sk-` keys through the agent: `ed25519-sk` and `ecdsa-sk` log in, tested in CI with OpenSSH's test provider (`ci/sk-agent.sh`)
- [x] Direct FIDO2 signing with `ctap-hid-fido2`, with no agent, on Linux and macOS; Windows signs through the agent (ADR 0027)
- [x] Keys bound to hardware through agents: Secure Enclave keys from Secretive, FIDO2 keys through the OpenSSH agent on Windows; a public key as `IdentityFile` picks the agent key (ADR 0028)
  - [x] Scope narrowed from "Keys bound to Secure Enclave (macOS) and Windows Hello" on 2026-10-24, approved by the maintainer: native keys move to v0.5
- [x] Zmodem receive: files a host sends with `sz`, asked first (ADR 0025)
- [x] Zmodem send: files chosen in the system dialog go to `rz` (ADR 0026)

### Exit criteria

- [x] Recording of a 1 hour session replays with seek under 200 ms
- [x] AI never sends terminal output off the machine without a per-host opt-in (test)
- [x] FIDO2 login tested with a YubiKey on all three operating systems
  - [x] Linux: `ed25519-sk` and `ecdsa-sk` log in to OpenSSH through the agent and through the security key path, with OpenSSH's test provider and a stand-in signing as it does (CI run 40085633808: `sk_agent` 6, `sk_direct` 7)
  - [x] Accepted on 2026-10-24 on the evidence above, by the maintainer: a login with a real YubiKey on Linux, macOS and Windows is still to be run, with [these steps](testing/fido2.md), when a key is at hand

## v0.5: Sync and import

Goal: Termius Pro sync without a Termius account.

### Scope

- [x] trzsz transfers, in Rust: `tsz` and `trz`, protocol 1 in base64, files only, through the Zmodem offer gate, checked live against pip trzsz 1.1.7 and trzsz-go 1.2.0 (ADR 0031)
- [x] End-to-end encrypted sync to a git remote, S3-compatible storage, or a synced folder: XChaCha20-Poly1305 under a key wrapped by Argon2id, which the vault already uses, in place of `age` (ADR 0030)
- [x] Sync set up, run, and reviewed from the desktop: a folder, git or S3 target and the sync key in the vault, a schedule, the last run's clashes and notices, and no host another device changed used before the user accepts it (ADR 0030)
- [x] Conflict handling with a CRDT: last-writer-wins per field on hybrid logical clocks, a delete winning over an edit, keyed lists per element, every clash listed on both devices; `automerge` and `yrs` weighed and not taken (ADR 0030)
- [x] Per-item flag to keep credentials off the sync target: each vault identity is shared only where its device turns it on, never by another device (ADR 0030)
- [x] Cloud import: AWS EC2 and Lightsail, DigitalOcean, Azure, GCP, through the user's own `aws`, `doctl`, `az` and `gcloud`, so Tauminus keeps no cloud key; a preview of what would be added, moved, brought back or marked gone comes before anything is saved, and a host is never deleted (ADR 0032; checked live with aws-cli 2.36 against LocalStack)
- [x] Import CSV, PuTTY, MobaXterm, SecureCRT, with their folders and jump hosts (ADR 0029)
- [x] Runbooks: snippets with variables shared through the git sync target: a multi-line snippet with `{{name:default}}` placeholders, in a folder, syncs sealed and fills on the other device (test: `a_runbook_with_variables_reaches_the_other_device_through_git_and_fills_there`); sharing with a team is the Hub's, in v0.6
- [ ] Code signing for macOS (Developer ID, notarization)
  - [x] The release signs and notarizes the macOS bundle when the `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_TEAM_ID`, `APPLE_API_ISSUER`, `APPLE_API_KEY` and `APPLE_API_KEY_P8` secrets exist, and builds unsigned otherwise
  - [ ] A Developer ID account, the secrets set, and a signed `.dmg` that opens on a Mac with no Gatekeeper warning
- [ ] Native keys bound to the Secure Enclave (signed builds, data protection keychain) and to Windows Hello (NCrypt, Passport provider), each after a spike on real hardware (moved from v0.4: ADR 0028)
- [ ] Auto-update with `tauri-plugin-updater`, signing key in CI secrets

### Exit criteria

- [x] Two devices edit the same host offline, then sync without data loss: both fields kept, or for one field both values shown as a clash (tests: `two_devices_edit_one_host_offline_and_sync_without_losing_either`, `the_device_whose_value_lost_is_told_on_its_next_sync`)
- [x] Sync target holds only ciphertext (tests: `the_target_holds_only_ciphertext` on a folder, and every object of a git remote through `git cat-file --batch-all-objects`)
- [ ] Update signature check rejects a tampered package (test)

## v0.6: Tauminus Hub

Goal: Termius Team and Business parity, self-hosted.

### Scope

- [ ] Hub server in Rust, single binary plus Postgres, Docker image
- [ ] Team vaults with roles (view, edit, admin), share hosts with or without credentials
- [ ] Revoke access with vault key rotation
- [ ] Require 2FA for members
- [ ] OIDC login on the client; SAML on the Hub
- [ ] SCIM provisioning
- [ ] Team session recordings with retention policy
- [ ] Audit log with export
- [ ] Admin-enforced prod guard rules
- [ ] Live terminal sharing through an end-to-end encrypted relay, one writer at a time
- [ ] API for inventory import (Ansible)

### Exit criteria

- [ ] Revoked member cannot decrypt data written after the revoke (test)
- [ ] Hub operator cannot read vault contents (threat model doc plus test)
- [ ] Security review of the Hub and the sharing relay

## v1.0 and later

- [ ] Web build: SSH in WASM, relay forwards only encrypted bytes
- [ ] iOS and Android through Tauri 2: extended keyboard, gestures, reconnect on resume
- [ ] Kerberos/GSSAPI, PKCS#11 smart cards, short-lived certificates from Vault or step-ca
- [ ] Kubernetes exec and Docker exec as connection types
- [ ] Credentials from 1Password, Bitwarden, HashiCorp Vault
- [ ] X11 forwarding
- [ ] RDP and VNC
- [ ] Windows code signing; Homebrew, winget, Flathub, AUR
- [ ] FIPS crypto mode

## Open risks

Tracked in [feature-parity.md](feature-parity.md#risks). The IPC throughput
risk is settled inside v0.1 by the benchmark exit criterion.
