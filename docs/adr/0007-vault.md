# ADR 0007: One local vault file, unlocked by password or keychain

- Status: accepted
- Date: 2026-10-22

## Context

Hosts need passwords and private keys that the user does not want to keep
in `~/.ssh` or type each time. Several hosts share one login, so a secret
belongs to an identity (a user, a password, a key, its passphrase), and
hosts or groups point at an identity. The secrets must stay encrypted on
disk and must never reach the webview, logs, or crash reports. Unlocking
should not need the password on every start when the OS offers a
keychain.

## Decisions

### One file, one data key, several wrapped copies

`vault.json` sits next to `hosts.json`. A random 256-bit data key encrypts
every record. The file stores that key only wrapped, once per slot:

- a password slot, wrapped by a key Argon2id derives from the master
  password (64 MiB, 3 passes, 1 lane: RFC 9106's second setting with one
  lane, about 0.3 s);
- an optional keychain slot, wrapped by a random 256-bit key that the OS
  keychain holds (macOS Keychain, Windows Credential Manager, Secret
  Service on Linux).

Changing the password also replaces the data key, so an old copy of the
file and the old password open nothing saved afterwards. The history's
keys are replaced with it (ADR 0008), so the same old copy opens nothing
the history records afterwards either. The Argon2id costs are raised to
at least the defaults. A keychain that was on gets a new key. Turning the keychain on asks for the master password again,
since the vault then opens without it. Turning it off removes its slot and
its entry; the data key stays until the next password change. The keychain is
written only after the file is saved, so its entry always opens the file
on disk.

### XChaCha20-Poly1305 per record, bound to its place

Each record has a fresh 192-bit random nonce, so random nonces do not
collide in practice. The additional data is the vault id plus the record
id, so moving a record to another id or into another vault fails
authentication. Slot wraps bind the vault id the same way, and the
password slot also binds the KDF costs and salt; the manifest binds them
too, so the keychain path refuses an edited header as well. A manifest sealed under
the data key lists each record's id and nonce; nonces change on every
save, so deleting a record or putting back an older one fails on open.
Replacing the whole file with an older copy is not detected: that needs a
counter kept outside the file.

AES-GCM-SIV was the alternative. XChaCha20-Poly1305 needs no AES hardware
for constant time, and its 192-bit nonce removes nonce bookkeeping.

### Bounds on everything read from the file

A tampered file could ask for a cheap KDF (downgrade) or a huge one
(denial of service). Reading refuses memory outside 19 MiB to 1 GiB,
passes outside 2 to 16, and more than 8 lanes. The floor is OWASP's
minimum. The file holds at most 4096 records of 64 KiB and 8 slots.

### One writer at a time

A save first checks that the file still holds what this window read,
under an exclusive lock on `vault.json.lock`, so check and rename are one
step across processes. If
another window saved in between (a new password, say), the save fails and
the vault locks, so the next unlock reads the newer file. Any failed save
locks the same way: memory never holds a change the file does not. A save
that would make the file larger than the 16 MiB read limit is refused.

### The open vault lives in the backend

The webview sends secrets in and gets back only flags (`hasPassword`,
`hasKey`) and the public key line. Secrets move into zeroizing buffers as
soon as they arrive. `Debug` on every type that holds one prints no secret.
A blank secret field on save keeps the stored secret; an empty string
removes it.

The vault locks itself after an idle time (15 minutes by default, 1 minute
to 24 hours). Typing and clicking count as activity, sent at most once per
30 s or a quarter of the idle time. Time asleep counts: the timer takes the
longer of the monotonic and the wall clock, and every use checks expiry
first, so waking the window cannot renew a vault that should have locked. Locking drops the data key and
all decrypted records. One Argon2id runs at a time.

### Connecting with an identity

`host_connect` looks up the nearest identity on the host or its groups.
While the vault is locked it fails with `vault_locked`, and the view offers
to unlock and retries once. The identity's user replaces the host's user.
Its key is tried before agent keys, since servers stop after six refused
keys. Its password answers the first password prompt, or a single hidden
keyboard-interactive field that names a password, once. A one-time code
prompt is left to the user. Both go only to the final hop: a jump host never sees them.

A stored password goes to whatever server the host entry names. Whoever
can edit the host list, or call `host_save` from the webview, could point
an identity's host elsewhere. That writer can already set a `ProxyCommand`,
which runs code, so binding identities to host keys would not lower the
risk; it is left out.

### Generated keys and installing them

Keys are generated with ssh-key (already in the tree through russh) from
the OS random source, on a blocking thread. Installing a public key runs a
POSIX `sh` script over an exec channel, signing in with the host's own
settings, not the identity being installed. The script is sent on stdin to
`exec sh -s`, which works whatever the login shell is; a server without a
POSIX `sh` (Windows OpenSSH) reports the failure.

## Consequences

- One vault per machine. Team vaults (v0.6, ADR 0034) seal their own
  keys to each member's devices.
- A user who forgets the master password and has no keychain slot loses the
  vault. The setup dialog says so.
- The keychain slot is as strong as the user's OS login. The settings say so
  next to the switch.
- Decrypted records, the history's keys, and history rows live in ordinary
  memory while the vault is open, and can reach swap or a crash dump; the
  SQLCipher key pages are not locked in memory where the OS limits it.
  A connect that took an identity's password or key before a lock keeps
  using it until that connect ends.
- Unlocking with the keychain can ask for Touch ID, Windows Hello, or polkit
  first (ADR 0009). That check guards the app's own unlock; keys the OS
  guards with biometrics wait for signed builds and a tested Hello slot.
