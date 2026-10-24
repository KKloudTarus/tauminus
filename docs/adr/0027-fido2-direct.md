# ADR 0027: FIDO2 keys signed by the security key, with no agent

- Status: accepted
- Date: 2026-10-24

## Context

OpenSSH `sk-` key files (`id_ed25519_sk`, `id_ecdsa_sk`) hold no secret.
They name a credential on a USB security key: the application (`ssh:`),
flags (touch, PIN), and a key handle. Since v0.4's first FIDO2 slice they
sign through ssh-agent only, so a user with no agent running, or with the
key file kept out of it, could not use them. A panel of two (security,
architecture) weighed where the CTAP code lives, how the signature is
built and checked, and how the user is asked to touch the key.

## Decisions

### A port of bytes, in a crate of its own

`tauminus-fido` holds the `SecurityKey` port: `get_assertion` takes the
application, the data to sign, the key handle, whether a touch is wanted,
and an optional PIN, and returns the authenticator data and the signature
as the key gave them. Nothing SSH crosses it. `crates/tauminus-ssh/src/sk.rs`
reads the key file, asks the port, and builds the SSH signature:

- the authenticator data must start with SHA-256 of the application, or
  the answer is refused;
- Ed25519 signatures are taken raw (64 bytes); ECDSA P-256 ones arrive as
  DER and become two mpints;
- the flags byte and the big-endian counter follow, as
  `sk-*@openssh.com` signatures carry them;
- the result is verified against the key file's public key before it is
  sent, so a key that answers for another credential ends this attempt.

Building and checking the signature is O(n) in the data signed, a few
hundred bytes per sign-in.

### USB through `ctap-hid-fido2`, Linux and macOS

`HidSecurityKey` speaks CTAP2 over HID with `ctap-hid-fido2` 3.6.0 (MIT,
pinned). With one key plugged in it is asked directly. With several, each
gets a silent assertion first (no touch, no PIN, as OpenSSH probes), and
only one that signs it is asked for real, so only it blinks. Any other
answer to the probe (busy, an option it lacks, a read error) passes the
key over: the PIN never goes to a key that has not shown it holds the
credential. With one key plugged in, the probe runs too before a PIN is
sent, so a wrong key does not count it against its own retries. A
credential that needs user verification just to be found (credProtect
level 3) answers the silent probe as unknown, so with several keys
plugged in it is not found; OpenSSH probes the same way. The library's
own keep-alive message is off; the app shows the touch.

One request runs at a time: two sign-ins at once would cross their HID
reports on one key, and one touch answers one of them. A request the
caller drops (the touch cancelled, the connect gone) is cancelled on the
key with `CTAPHID_CANCEL`, sent between two keep-alive reads, so the key
stops waiting within about 200 ms instead of at its own timeout, and the
next sign-in finds it free. A key file without the touch flag gets 30 s
to sign.

The key file's application must start with `ssh:`, as OpenSSH enrolls
them, or the key is not asked: a file naming a web site's credential
cannot get it signed for that site.

Windows lets only `webauthn.dll` reach a security key from a process that
is not elevated. There the adapter is not built, and FIDO2 keys keep
signing through the agent (Windows' OpenSSH agent supports them). A
`webauthn.dll` adapter is left for later.

On Linux, `hidapi` links `libudev.so.1` at load time, which every
systemd distribution ships (WebKitGTK already needs it); CI installs
`libudev-dev` to build. Access to `/dev/hidraw*` comes
from systemd's `uaccess` rule for devices its `fido_id` marks as security
tokens (systemd 244 and later).

### Asking the user, and withdrawing the question

Two prompts join the `Prompt` port: `SecurityKeyPin`, asked first when
the key file's flags want user verification, and `SecurityKeyTouch`,
shown while the key waits. A key set to always verify (CTAP 2.1
`alwaysUv`) wants its PIN though the file's flags do not: its
`PIN_REQUIRED` gets the PIN asked and one more request, as OpenSSH does. The touch prompt and the signing run in one
`tokio::select!`, signing first: when the key answers, the prompt's future
is dropped. Dropping a prompt future now withdraws it: the desktop sends
`withdrawn` with the prompt's id, and every prompt channel in the view
(terminal, SFTP, forwards, key install) aborts that question and hides
it. Its late answer, if any, goes nowhere. The user's Cancel on the touch
prompt ends the attempt and cancels the request on the key.

### Failures are named

A key that does not sign sends a signature the server refuses, so
authentication moves on to the next method as with an agent that
declines. The failed sign-in names why: no security key plugged in, a key
that does not hold the credential, a refused or blocked PIN, a touch not
given in time or cancelled, a key that did not answer. A file whose key
failed is not asked again in the same sign-in.

## Consequences

- An `sk-` key file signs with no agent on Linux and macOS. The methods
  keep their order: a vault identity's key, then the agent (which may
  hold the same key), then key files.
- 22 crates join the tree through `ctap-hid-fido2` (CBOR, X.509 parsing
  for attestation), all MIT or Apache-2.0; `cargo deny` passes.
- CI runs the whole path against OpenSSH with a stand-in key that signs
  as OpenSSH's `sk-dummy.so` does (`crates/tauminus-ssh/tests/sk_direct.rs`):
  both key types log in, and a missing key, an unknown credential and a
  cancelled touch fail with their reasons; the cancelled touch reaches the
  key as a cancel. The USB transport itself is
  tested only with a real key, which the v0.4 exit criterion covers.
