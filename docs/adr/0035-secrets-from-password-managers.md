# ADR 0035: Secrets from password managers

- Status: accepted
- Date: 2026-10-27

## Context

People who run servers keep logins in 1Password, Bitwarden or
HashiCorp Vault. Copying them into the Tauminus vault (ADR 0007) makes a
second copy to keep in step and to lose. `docs/feature-parity.md` ranks
"credentials from 1Password, Bitwarden, HashiCorp Vault at connect time"
ninth among features beyond Termius.

Today `Login::resolve` reads an identity's password, key and passphrase
from the vault before the connection opens, and the authenticator
spends them once the server's key is accepted.

A panel of three weighed two designs on frozen criteria: security 35%,
usability 25%, complexity and dependencies 20%, cross-platform 10%,
testability 10%. One advocate argued for each manager's own CLI; one
for the managers' SSH agents and their HTTP APIs; a security review
wrote the properties either had to meet.

What the panel found:

- Keys are best left in the manager: 1Password and Bitwarden both run
  an SSH agent that signs without the key leaving it, and asks the user
  per client. Tauminus already speaks to any agent through
  `IdentityAgent`, with the socket owner checked (ADR 0028).
- Passwords over HTTP need a token Tauminus would keep: 1Password
  Connect and Bitwarden Secrets Manager tokens do not expire on their
  own. Bitwarden's password manager has no HTTP API but `bw serve`,
  which any local process may call. 1Password's SDKs are Go,
  JavaScript and Python.
- The CLIs keep the user's own sign-in: `op` unlocks through the
  1Password app, `vault` reads the token `vault login` left in
  `~/.vault-token`, `bw` reads `BW_SESSION`. ADR 0032 already runs
  users' CLIs this way for cloud import.
- Run at the wrong moment, a CLI fetch hands a secret to a server
  whose key was never checked, and wakes an unlock prompt for a
  connection that fails anyway.

## Decision

### References, not secrets

An identity's password, private key and passphrase may each be a
reference instead of a stored secret:

- 1Password: `op://vault/item/[section/]field`, read with
  `op read --no-newline -- <reference>`. A private key reference adds
  `?ssh-format=openssh`.
- Bitwarden: an item's id, read with
  `bw get password <id> --nointeraction`, for a password or a
  passphrase. Keys come through Bitwarden's SSH agent instead.
- HashiCorp Vault: an address, a KV mount, a path and a field, read
  with `vault kv get -address=<address> -mount=<mount> -field=<field>
  -- <path>`, which reads KV version 1 and 2 alike.

Each reference is checked against a narrow grammar before it is saved
and again before it runs: no leading `-`, no `..`, only characters that
mean nothing to a shell or to `cmd.exe`, a Bitwarden id that is a UUID,
a Vault address that is `https`, or `http` to a loopback address. The
reference lives in the identity's sealed record; no command takes one
from the view to fetch, and nothing fetched goes back to the view,
which sees only which slots hold a reference and what it names.

An identity that holds a reference is not shared through sync yet: a
reference names something in one person's account.

### Fetched when spent

The connection resolves a reference only after the server's key is
accepted, only for the last hop, and only when the server offers a
method that spends it: the password when it allows password or
keyboard-interactive, the key and its passphrase when it allows
public keys, including after a partial success opens that method. The
fetched secret lives for that sign-in and is wiped when it ends; a
reconnect fetches again. Sign-ins asking for the same secret at once
share one run of the CLI, and at most two CLIs run at a time, so a
reconnect of many panes starts a few processes, not one each. A
manager that gives nothing leaves the other methods to try; its error
is the one shown if none gets in. Password fill (ADR 0021)
fetches the same way, when the user asks for it.

### Running the CLIs

The CLIs are found and run as ADR 0032 runs the cloud CLIs, through a
runner the two now share: absolute `PATH` entries and the installers'
own folders, a fixed argument list, no shell, no input, killed when the
connection gives up, a deadline (120 s, longer than an unlock prompt),
and stdout read into a buffer reserved whole up front and wiped, capped
at 64 KiB. On Windows `.exe` goes before `.cmd`, and no folder under
`%ProgramData%` is searched, since any user may make folders there. A CLI that would ask on a terminal
fails instead, and the user sees its first error line with how to sign
in: `op signin`, `bw unlock`, `vault login`.

## Consequences

- Tauminus keeps no password-manager token. A manager that is locked
  fails the sign-in with the manager's own message.
- A dock-launched app has no `BW_SESSION`, so Bitwarden passwords work
  when Tauminus is started from a shell that unlocked `bw`. Tauminus
  never takes the Bitwarden master password.
- 1Password's app approves a process for ten idle minutes, during
  which any reference resolves: the references in the vault are the
  only bound on what Tauminus reads. Adding or changing one is a change
to a secret: the backend asks the user in a native dialog first, naming
each reference in full, and for Vault the address its sign-in token
goes to. The identity's name in that dialog holds no control, format or
line-breaking character, and the save is refused if the references
changed since the user answered.
- The CLI that runs receives the secret it prints. Whoever can put a
  file earlier in `PATH` than the real CLI already runs code as the
  user; the identity editor shows which file runs.
- An identity that holds a reference is unreadable to a build from
  before this change: its record has a field that build refuses.
- Tested with a fake CLI for each manager, and live against Vault
  1.20.4's dev server (KV versions 1 and 2, a key, a missing path).
  Bitwarden and 1Password are covered by the fakes alone: neither runs
  without an account.
