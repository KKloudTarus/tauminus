# ADR 0028: Keys bound to hardware through agents; native binding after signed builds

- Status: accepted
- Date: 2026-10-24

## Context

v0.4 lists keys bound to the Secure Enclave (macOS) and Windows Hello: SSH
keys whose private half never leaves the machine's hardware. A panel of
three (an advocate for native code now, an advocate for agents, a
security review) weighed it on frozen criteria: security of the binding
30%, testability without the hardware 25%, risk of shipping a hardware
path that never ran 20%, build cost 15%, user value 10%.

Facts the panel worked from:

- macOS releases are not code-signed. Security.framework keeps a Secure
  Enclave key persistently only in the data protection keychain, which
  needs a signed app with a keychain access group (ADR 0009 turned an
  option down for the same reason). CryptoKit's `dataRepresentation`
  avoids the entitlement but is Swift-only, and Apple documents it as
  bound to the device, not to the app: any process of the same user that
  copies the blob can sign with it on that Mac.
- Windows' `KeyCredentialManager` (Hello) signs with RSA-PSS, which SSH
  has no algorithm for (`rsa-sha2-256` is PKCS #1 v1.5). NCrypt with the
  Passport key storage provider could sign PKCS #1 v1.5, but an
  unpackaged app's key is found by name by any process of the same user,
  and without a TPM the key lives in software.
- No runner can create such a key: GitHub's macOS runners are VMs, and
  Windows runners answer "no Windows Hello device" (`docs/testing/presence.md`).
- Users already get keys held by hardware through Tauminus: FIDO2 `sk-`
  keys sign directly on Linux and macOS (ADR 0027) and through agents
  everywhere; Secretive, a signed app, keeps Secure Enclave keys and
  serves them as `ecdsa-sha2-nistp256` through an agent socket.

## Decision

v0.4 delivers keys bound to hardware through agents, tested, and moves
native binding to v0.5, after macOS builds are signed.

- **Secure Enclave through Secretive.** Its documented setup points
  `IdentityAgent` at its socket, and names the public key as
  `IdentityFile` with `IdentitiesOnly`. Tauminus had neither:
  - `IdentityAgent` is now a host setting, inherited through groups like
    the others, read from `~/.ssh/config` and editable in the host form:
    `none`, `SSH_AUTH_SOCK`, an absolute path, or `$NAME`, kept as a name
    and read when connecting, as ssh does. An unset variable means no
    agent: falling back to the default one would offer and forward keys
    the config kept out. A relative path or a bad name is refused. The
    agent named signs on every hop and is the one forwarded. The hosts
    file moves to version 11 only when a host or group sets it. `ssh -G`
    prints `${NAME}`, `~` and `%d` already read, so those arrive as the
    path they held at import; the built-in reader, used without `ssh`,
    refuses `%` tokens and a host with one is reported, not imported.
  - What answers at that path must be the user's own, since a host's
    setting (or a compromised view) may name any socket. On Unix the
    listener's credentials are checked after connecting, so root's
    `docker.sock` or another user's socket is refused; on Windows the
    pipe must belong to the user, where the default agent's pipe may also
    belong to the system.
  - A forwarded channel carries agent requests only: listing keys (11),
    signing (13) and extensions (27), each up to 256 KiB, one at a time.
    Adding or removing keys, locking, and bytes of any other protocol are
    answered with a failure or end the channel, and never reach the
    socket.
  - An identity file may be a public key. Under `IdentitiesOnly` it picks
    that key among the agent's; before, the agent's key was passed over
    and the sign-in failed. It is never offered on its own, since nothing
    here signs for it.
- **Windows.** FIDO2 `sk-` keys sign through the OpenSSH for Windows
  agent, which reaches them through `webauthn.dll`. Whether that agent
  accepts Windows Hello's platform authenticator in place of a USB key is
  unverified; `docs/testing/hardware-keys.md` asks.
- **Tested key types.** Agent sign-in is tested with Ed25519, ECDSA P-256
  and P-384, and RSA against OpenSSH, and with Ed25519 and P-256 through
  the OpenSSH agent service and Pageant on Windows.
- **Native binding, v0.5.** On macOS, once releases are signed:
  Security.framework keys in the data protection keychain under
  Tauminus's access group, with `SecAccessControl` asking for presence on
  each signature. On Windows, NCrypt with the Passport provider, padding
  PKCS #1 v1.5, a random key name, and enrollment refused unless the key
  reports hardware. Both start with a spike on real hardware, and both
  require a second key enrolled on the server, since a key that cannot
  leave the machine dies with it.

## Consequences

- No hardware path ships that has never run. Every assertion of this
  decision runs in CI.
- A Secretive user's `~/.ssh/config` works in Tauminus as it does in
  `ssh`.
- Through a forwarded agent, a server lists keys and gets signatures, as
  `ssh`, `git` and `ssh-add -l` need; `ssh-add` there can no longer add
  or remove keys in the local agent, which OpenSSH allows.
- A Windows user with no USB key gets no key bound to Hello until v0.5,
  unless the OpenSSH agent's `webauthn.dll` path accepts the platform
  authenticator.
