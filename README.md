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

Pre-alpha. Nothing is usable yet.

## Roadmap

| Version | Scope |
|---|---|
| v0.1 | Local shell, SSH (password, key, agent, ProxyJump), `known_hosts`, import `~/.ssh/config`, tabs and panes, command palette |
| v0.2 | Prod guard, searchable history, encrypted vault |
| v0.3 | SFTP, port forwarding, snippets, multi-host commands, auto-reconnect |
| v0.4 | Bring-your-own-storage sync, team sharing, audit log export |
| v1.0+ | Web build (SSH in WASM over an encrypted relay), mobile |

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
