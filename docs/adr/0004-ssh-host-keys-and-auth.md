# ADR 0004: SSH host keys and authentication

- Status: accepted
- Date: 2026-10-21

## Context

An SSH client earns trust in two places: deciding that a server is the one
the user meant, and proving who the user is without leaking secrets. OpenSSH
is what users know and what their files are written for. Each rule below was
compared with OpenSSH 9.6 behaviour on a live server and with `hostfile.c`,
`sshconnect.c`, and `sshconnect2.c` on 2026-10-21, after a review found three
places where an earlier draft was weaker than OpenSSH.

## Decisions

### Library: russh 0.64 on ring

`russh` is the maintained pure-Rust SSH client; `ssh2` binds libssh2 in C and
`thrussh` is unmaintained. The `ring` backend replaces the default
`aws-lc-rs`, a C library that needs CMake and NASM to build on Windows. SHA-1
`ssh-rsa` host keys are not offered, as in OpenSSH 8.8 and later; RSA keys
still work through `rsa-sha2-256` and `rsa-sha2-512`.

### A mismatch of any key type is a changed key

OpenSSH reports `HOST_CHANGED` whenever a recorded plain key does not match,
whatever its type. Asking instead would give an interceptor a trust prompt for
the price of offering a type that is not on record. To keep honest servers
from tripping this, the client offers the recorded key types first, as
`order_hostkeyalgs` does.

### Revocation is final

`@revoked` stops the connect for a plain key, for the key inside a host
certificate, and for the certificate's authority. A certificate that fails
any other check (principal, validity window, type, critical options) falls
back to its plain key, as in OpenSSH.

### A missing certificate is flagged

When an `@cert-authority` covers a host but it presents no certificate that
checks out, the first-connect prompt says so in red. OpenSSH only prompts;
fleets that rely on certificates rarely see a plain-key prompt, so it is
worth a warning.

### Unreadable files fail closed

A `known_hosts` file that exists but cannot be read stops the connect.
Skipping it would turn a changed key into a new one. A line that is not UTF-8
hides only itself.

### Tauminus writes only its own file

Keys the user trusts are appended, hashed, to `known_hosts` in the app's
config directory, created with mode 0600. The user's `~/.ssh` files are read
and never written.

### Passphrases only for keys the server accepts

Key files are offered by their public half (stored in the clear in the
OpenSSH format, or read from `.pub` next to older PEM keys). The server asks
for a signature only after accepting a key, and only then is the key
decrypted. When the user gives up on a passphrase or an agent refuses to
sign, the client sends a well-formed signature the server rejects, because
russh waits for a signature once one was requested; it costs one attempt
against `MaxAuthTries`.

### Timeouts do not count the user

The 15 s connect limit covers TCP, the proxy, and the handshake until the host
key check starts. Reading a fingerprint has its own 10 minute limit in the
prompt. A connect that is abandoned cancels its open prompt, so a late answer
cannot record a key for a connection nobody uses.

## Consequences

- A host whose key type changed needs its old line removed, as with OpenSSH.
- Jump hosts receive the same credentials as the destination until per-host
  settings arrive with the hosts store. The inner host key check still stops
  a hostile jump host from impersonating the destination.
- RUSTSEC-2023-0071 is ignored in `deny.toml`, with the reasoning there.
