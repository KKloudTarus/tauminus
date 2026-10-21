# Tauminus

Open-source SSH terminal for people who run production servers.

Tauminus is a desktop app built with Rust and Tauri. It runs local shells and
SSH sessions in the same window, and it targets three problems other terminals
leave open:

- **Prod guard.** Hosts tagged `prod` get a red frame. Dangerous commands
  (`rm -rf`, `DROP TABLE`, `kubectl delete`, `shutdown`) need confirmation
  before they are sent.
- **Searchable history across hosts.** Commands and output from every session
  are stored locally, encrypted, and searchable from one box.
- **Local-first.** No account. Sync, when it lands, goes through storage you
  choose (git, S3, iCloud, Syncthing) with end-to-end encryption.

## Status

Pre-alpha. The skeleton opens a local shell in a terminal window. SSH, hosts,
and the prod guard are next, per the roadmap below.

## Development

```sh
pnpm install
pnpm exec playwright install chromium
pnpm dev            # run the app
cargo xtask ci      # every check CI runs
```

Setup details and contribution rules: [CONTRIBUTING.md](CONTRIBUTING.md).
Code layout and data flow: [docs/architecture.md](docs/architecture.md).

## Roadmap

| Version | Goal | Scope |
|---|---|---|
| v0.1 | Daily driver | Local shell, SSH (password, keys, certificates, agent, ProxyJump, ProxyCommand, proxies), `known_hosts`, `~/.ssh/config` import, tabs, panes, command palette |
| v0.2 | Prod guard and memory | Prod guard, searchable history, encrypted vault, identities, biometric unlock |
| v0.3 | Termius Starter parity | SFTP, port forwarding, snippets with variables, broadcast, workspaces, auto-reconnect, Telnet, serial, Mosh |
| v0.4 | Beat Termius Pro | Session recording and replay, triggers, autocomplete, AI with command approval, FIDO2, hardware-bound keys |
| v0.5 | Sync without an account | End-to-end encrypted sync to git, S3, or a folder; cloud and app imports; auto-update |
| v0.6 | Team features | Self-hosted Tauminus Hub: team vaults, roles, revocation, OIDC/SAML, SCIM, audit log, live sharing |
| v1.0+ | More platforms | Web, mobile, Kerberos, smart cards, Kubernetes, RDP |

Details and exit criteria: [docs/roadmap.md](docs/roadmap.md).
Feature comparison with Termius and others: [docs/feature-parity.md](docs/feature-parity.md).
Decisions: [docs/adr/](docs/adr/).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
