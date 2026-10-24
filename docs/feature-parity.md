# Feature parity

What Termius ships, what other terminals ship that Termius does not, and how
Tauminus covers each item. Sources were read on 2026-10-21. Library versions are
the latest on crates.io or npm on that date.

Status values:

- **ready**: a maintained library covers it.
- **glue**: the library has the primitive, Tauminus writes the wiring.
- **build**: no usable library, Tauminus writes it.
- **hub**: needs the Tauminus Hub server (see [ADR 0002](adr/0002-hub-server.md)).
- **risk**: open technical question, see the risk list at the end.

## Termius pricing

Annual billing, from https://termius.com/pricing.

| Tier | Price |
|---|---|
| Starter | Free, commercial use allowed |
| Pro | $10/mo ($15/mo monthly on the US App Store) |
| Team | $20/seat/mo |
| Business | $30/seat/mo, SAML SSO is a paid add-on |
| Enterprise | Custom |

## Termius features

"Tier" is the lowest Termius tier that has the feature.

### Connections

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| SSH | Starter | `russh` 0.64.1 | ready | v0.1 |
| Local terminal | Starter | `portable-pty` 0.9.0 | ready | v0.1 |
| Jump host chain (ProxyJump) | Starter | `direct-tcpip` channel into `russh::client::connect_stream` | glue | v0.1 |
| SOCKS5 / HTTP proxy | Starter | `tokio-socks` 0.5.3, HTTP CONNECT written by hand | glue | v0.1 |
| ProxyCommand | Not in Termius | Child process stdin/stdout as the transport stream | glue | v0.1 |
| Agent forwarding | Starter | `russh` agent forwarding channels | ready | v0.2 |
| Environment variables per host or group | Starter | `russh` `set_env`; server needs `AcceptEnv` | ready | v0.2 |
| Local port forwarding | Starter | `russh` `channel_open_direct_tcpip` | ready | v0.3 |
| Remote port forwarding | Starter | `russh` `tcpip_forward` | ready | v0.3 |
| Dynamic (SOCKS) forwarding | Starter | `fast-socks5` 1.0.0 server opening `direct-tcpip` per connection | glue | v0.3 |
| Telnet | Starter | RFC 854 + NAWS, TTYPE, ECHO on tokio (about 400 LOC) | build | v0.3 |
| Serial | Starter | `serialport` 4.10.1, `tokio-serial` 5.5.0 | ready | v0.3 |
| Mosh | Starter | Upstream `mosh-client` inside a PTY on macOS and Linux | risk | v0.3 |
| Auto-reconnect | Starter | Reconnect loop plus `tmux` reattach | build | v0.3 |
| X11 forwarding | Not in Termius | `russh` `request_x11` plus a bridge to the local display | glue | v1.0+ |

### Authentication and keys

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Password | Starter | `russh` `authenticate_password` | ready | v0.1 |
| Keyboard-interactive (OTP, 2FA) | Starter | `russh` `authenticate_keyboard_interactive_*` | ready | v0.1 |
| Keys: Ed25519, ECDSA, RSA-SHA2 | Starter | `russh` `authenticate_publickey` | ready | v0.1 |
| OpenSSH user certificates | Starter | `russh` `authenticate_openssh_cert` | ready | v0.1 |
| System ssh-agent and Pageant | Not in Termius | `russh` `AgentClient` (`connect_uds`, `connect_named_pipe`, `connect_pageant`) | ready | v0.1 |
| `known_hosts` with wildcards, `@cert-authority`, `@revoked` | Starter | Own parser; `russh` matches exact hosts only | build | v0.1 |
| Post-quantum key exchange (`mlkem768x25519`) | Starter | `russh` default | ready | v0.1 |
| Key generation and import with passphrase | Starter | `ssh-key` 0.6.7 | ready | v0.2 |
| Export public key to `authorized_keys` | Starter | exec over an authenticated session | glue | v0.2 |
| Identities (user, password, key bundles) | Starter | Domain model in the vault | build | v0.2 |
| App lock (PIN, biometrics) | Starter | `tauri-plugin-biometric` 2.5.0 (macOS, iOS, Android), `robius-authentication` 0.3.1 for Windows Hello | glue | v0.2 |
| FIDO2 keys (`ed25519-sk`, `ecdsa-sk`) | Starter | Through an agent today; direct signing needs a `Signer` on `ctap-hid-fido2` 3.6.0 | risk | v0.4 |
| Keys bound to Secure Enclave, Windows Hello, Android Keystore | Starter | `security-framework` 3.7.0, Windows KeyCredential | risk | v0.4 |
| ML-DSA keys | Starter | Waits on `ssh-key` support | risk | v1.0+ |

### Vault and sync

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Local encrypted vault | Starter | Argon2id + ChaCha20-Poly1305, key in OS keychain (`keyring`) | build | v0.2 |
| Personal sync, end-to-end encrypted | Pro | `age` 0.12.1 over storage the user picks: git (`git2` 0.21.0), S3 (`object_store` 0.14.2), a synced folder | build | v0.5 |
| Keep credentials off the sync target | Pro | Per-item sync flag | build | v0.5 |
| Team vault | Team | Hub | hub | v0.6 |
| Share a host with or without credentials | Team | Hub | hub | v0.6 |
| Vault roles (edit, view) | Team | Hub | hub | v0.6 |
| Grant and revoke access | Team | Hub, with vault key rotation on revoke | hub | v0.6 |
| Require 2FA for the team | Team | Hub | hub | v0.6 |
| Multiple vaults with per-vault access | Business | Hub | hub | v0.6 |
| SAML SSO | Business add-on | OIDC on the client (`openidconnect` 4.0.1); SAML terminates on the Hub | hub | v0.6 |
| Team session logs | Team | Hub stores encrypted recordings | hub | v0.6 |
| Custom log retention | Enterprise | Hub policy | hub | v0.6 |
| SCIM provisioning | Not in Termius | Hub | hub | v0.6 |
| Audit log export | Not in Termius | Hub | hub | v0.6 |
| Self-hosted server | Not in Termius | Hub is self-hosted by design | hub | v0.6 |

### Terminal

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Tabs | Starter | React | build | v0.1 |
| Split panes (Termius caps at 16) | Starter | `react-resizable-panels` 4.14.8 | ready | v0.1 |
| Command palette | Starter | `cmdk` 1.1.1 | ready | v0.1 |
| Themes per host or group | Starter | Shared design tokens for UI and `xterm.js` | build | v0.1 |
| Fonts (Nerd Font set) | Starter | `xterm.js` font options | ready | v0.1 |
| Custom keyboard shortcuts | Starter | Keymap layer | build | v0.1 |
| OSC 52 clipboard, OSC 8 links | Starter | `xterm.js` addons | ready | v0.1 |
| Character encoding | Starter | Decode in Rust before the IPC channel | glue | v0.3 |
| Workspaces (saved layouts) | Starter | Layout tree in the store | build | v0.3 |
| Broadcast input to all panes | Starter | Fan-out in the session layer | build | v0.3 |
| Session status dots (unread, needs input, error) | Starter | Session events | build | v0.3 |
| Autocomplete (commands, paths, arguments) | Starter | Forked Fig specs (`@withfig/autocomplete` 2.692.3, frozen since 2025-05) | build | v0.4 |
| Password suggestion at prompts | Starter | Prompt detection plus identities | build | v0.4 |
| AI: text to command, snippet from description | Starter, account required | User key or local model, off by default | build | v0.4 |

### Snippets and history

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Shell history | Starter | SQLite (`rusqlite` + FTS5) | build | v0.2 |
| Snippets and folders | Starter | Store plus palette | build | v0.3 |
| Run a snippet on many hosts | Starter | Session fan-out | build | v0.3 |
| Snippet variables | Not in Termius | `{{name}}` placeholders, filled from a prompt or a shell command | build | v0.3 |
| Startup snippets | Pro | Run after shell ready | build | v0.3 |
| Session logs with bookmarks and comments | Pro | Recording store (asciicast) | build | v0.4 |
| Search session logs | Pro | FTS5 over recordings | build | v0.4 |

### Hosts and import

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Groups with inherited settings | Starter | Domain model | build | v0.1 |
| Tags and tag filter | Starter | Domain model | build | v0.1 |
| Import `~/.ssh/config` | Starter | `ssh -G` when OpenSSH exists, else `ssh2-config` 0.9.0 (no `Match`, no tokens) | glue | v0.1 |
| Import CSV | Starter | Parser | build | v0.5 |
| Import PuTTY, MobaXterm, SecureCRT | Starter | Parsers | build | v0.5 |
| AWS (EC2, Lightsail) sync | Pro or Team (Termius sources disagree) | `aws-sdk-ec2` 1.269.0 | ready | v0.5 |
| DigitalOcean sync | Pro or Team | REST via `reqwest` | glue | v0.5 |
| Azure sync | Pro or Team | ARM REST with `azure_identity` 1.0.0 | glue | v0.5 |
| GCP sync | Not in Termius | `google-cloud-compute-v1` 4.0.0 | ready | v0.5 |
| API bridge, Ansible inventory import | Team | Hub API | hub | v0.6 |

### SFTP

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| SFTP dual pane | Starter | `russh-sftp` 3.0.1 | ready | v0.3 |
| Drag and drop from the OS | Starter | Tauri drag-drop event | glue | v0.3 |
| Edit remote file, re-upload on save | Starter | Temp file plus watcher | build | v0.3 |
| chmod and permissions | Not confirmed in Termius | `russh-sftp` `set_metadata` | ready | v0.3 |
| SCP | Not in Termius | Skipped: OpenSSH 9.0+ `scp` uses SFTP | n/a | n/a |

### Collaboration

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Live terminal sharing (one writer at a time) | Starter per pricing table | Relay on the Hub, end-to-end encrypted | hub | v0.6 |

### Mobile

| Feature | Tier | Tauminus approach | Status | Version |
|---|---|---|---|---|
| Extended keyboard, gestures | Starter | Tauri 2 mobile UI | risk | v1.0+ |
| Background sessions | Starter (iOS limited to about 20 to 30 s) | Reconnect on resume, Android foreground service | risk | v1.0+ |
| Voice input, paste image to `/tmp` | Starter | Platform APIs plus SFTP | build | v1.0+ |

### Removed or retired by Termius

- Port knocking: removed in 2022.
- MultiKey: retired on 2026-10-15, replaced by SSH ID.

Tauminus does not plan either. SSH ID is a Termius account service and has no
equivalent outside Termius.

## Beyond Termius

Features found in other tools and missing from Termius, ranked by value for
people who run production servers.

| Rank | Feature | Seen in | Tauminus approach | Version |
|---|---|---|---|---|
| 1 | Prod guard: red frame, confirmation for dangerous commands, admin-enforced denylist | Warp (agent commands only) | Shell parsing with `brush-parser` 0.4.0 and `tree-sitter-bash` 0.25.1 | v0.2 |
| 2 | Session recording with replay and search | Teleport, Tailscale, Guacamole | asciicast v2 writer (NDJSON, under 100 LOC), FTS5 index | v0.4 |
| 3 | AI agent with command approval and regex allow/deny lists | Warp, Wave, Electerm | Same guard engine as rank 1 | v0.4 |
| 4 | Sessions that survive sleep and roaming, `tmux` control mode | Wave, iTerm2 | Reconnect plus `tmux -CC` parser | v0.3 / v0.4 |
| 5 | Triggers and regex highlight sets | iTerm2, SecureCRT, Xshell | Output scanner in Rust | v0.4 |
| 6 | Runbooks and parameterized workflows shared through git | Warp Drive | Snippet variables plus git sync | v0.5 |
| 7 | Kerberos/GSSAPI, PIV smart cards, short-lived certificates from Vault or step-ca | SecureCRT, Xshell, Teleport | `russh` `GssapiAuthenticator`, `cryptoki` 0.12.1 | v1.0+ |
| 8 | Inventory plugins: GCP, Kubernetes, Docker, custom scripts | Royal TSX, Teleport, Tabby | `kube` 4.2.0, `bollard` 0.21.1 | v1.0+ |
| 9 | Credentials from 1Password, Bitwarden, HashiCorp Vault at connect time | Royal TSX | Provider CLIs or APIs | v1.0+ |
| 10 | Zmodem and trzsz, serial hex view, instant Ctrl+C on broadcast | SecureCRT, Xshell, Electerm | `zmodem2` 0.7.3 in Rust (ADRs 0025, 0026); trzsz in Rust later, not the npm library, whose bytes would cross the webview | v0.4, trzsz v0.5 |

Lower priority, tracked for later: RDP (`ironrdp` 0.17.0), VNC (`vnc-rs` 0.6.0),
scripting API, FIPS crypto mode (`aws-lc-rs` FIPS build), per-line timestamps,
paste safety prompt, search across all tabs.

## Verified by experiment

A spike on 2026-10-21 ran `russh` 0.64.1 and `russh-sftp` 3.0.1 against two
OpenSSH 10.6p1 servers on Linux, one acting as a jump host. All 11 checks passed.

| Check | Result |
|---|---|
| `known_hosts` match | pass |
| `known_hosts` changed key | rejected with "server key changed at line 1" |
| Ed25519 public key | pass |
| OpenSSH user certificate | pass; `sshd` logged `ED25519-CERT ... CA ED25519` |
| ssh-agent | pass |
| PTY shell and `window_change` | pass; `stty size` reported `40 132` |
| 256 MiB stdout | 263 to 278 MiB/s in 32 KiB chunks; OpenSSH client measured the same way: 198 to 216 MiB/s |
| Local forward (`direct-tcpip`) | pass |
| ProxyJump | pass |
| Remote forward (`tcpip-forward`) | pass |
| SFTP 4 MiB write, read, list | 64 ms |

Not covered: password and keyboard-interactive (the lab `sshd` ran without root
and without PAM), FIDO2, Windows, macOS.

## Risks

1. **IPC throughput into `xterm.js`.** Tauri sends `Channel` payloads under
   1024 bytes as a JSON number array and larger ones as raw bytes over `fetch`.
   PTY output must be coalesced to at least 1 KiB. Not yet measured on
   WebKitGTK, WKWebView, or WebView2.
2. **`ssh_config` fidelity.** `ssh2-config` lacks `Match` and tokens. `ssh -G`
   covers machines with OpenSSH installed.
3. **Hardware-backed auth.** FIDO2 direct signing, PKCS#11, and GSSAPI each need
   a custom `Signer` or authenticator, tested on three operating systems.
4. **Biometric key binding.** macOS Secure Enclave needs a signed build with
   entitlements. Linux has no hardware binding.
5. **Mobile and Mosh.** iOS suspends sockets in the background. Rust Mosh
   clients are pre-alpha. Windows has no upstream `mosh-client`.
6. **`portable-pty` maintenance.** Last release 0.9.0 on 2025-02-11. Pin and
   be ready to vendor.

## Sources

- Termius: https://termius.com/pricing, https://docs.termius.com, desktop, iOS,
  and Android changelogs under https://docs.termius.com/changelog/
- Warp: https://www.warp.dev/pricing, https://docs.warp.dev
- Teleport: https://goteleport.com/docs/
- Tailscale SSH recording: https://tailscale.com/kb/1246/tailscale-ssh-session-recording
- iTerm2: https://iterm2.com/features.html
- SecureCRT: https://www.vandyke.com/products/securecrt/features.html
- Xshell: https://www.netsarang.com/en/xshell-all-features/
- MobaXterm: https://mobaxterm.mobatek.net/features.html
- Royal TS: https://www.royalapps.com/ts/mac/features-all
- Wave: https://docs.waveterm.dev
- Electerm: https://github.com/electerm/electerm
- Tauri IPC: https://v2.tauri.app/develop/calling-frontend/
