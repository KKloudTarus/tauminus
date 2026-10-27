# ADR 0034: Team vaults

- Status: accepted
- Date: 2026-10-27

## Context

v0.6 brings team vaults to the Hub (ADR 0002): shared hosts, snippets
and credentials, roles (view, edit, admin), sharing a host without its
credentials, and revoking a member with key rotation. Its exit criteria:
a revoked member cannot decrypt data written after the revoke, and the
Hub operator cannot read vault contents. A synced host carries
`ProxyCommand` and startup snippets (ADR 0030), so whoever can write a
host runs code on every member's machine: who wrote an item matters as
much as who can read it.

A panel of three weighed two designs on frozen criteria: security
against a malicious Hub 35%, revocation 20%, complexity and dependencies
20%, offline clients 15%, performance at 1000 members with 5 devices in
100 vaults 10%. One advocate argued for per-vault keys in epochs wrapped
to each device under an admin-signed log, one for MLS (RFC 9420), and a
security review wrote the threat model both had to meet.

What the panel found:

- Both fail the same cheap way: a Hub that hides a revoke from one
  writer gets that writer to seal new items under the old key, which the
  revoked member holds. MLS does not prevent it; both need the same
  freshness rule.
- MLS gives a joining member no past epochs. A new member must read
  what the vault already holds, so MLS would need the per-device key
  wrapping of the other design built on top of it.
- `openmls_rust_crypto` 0.6.0 pins `chacha20poly1305` 0.10,
  `ed25519-dalek` 2 and `p256` 0.13, a second copy of each beside the
  tree's 0.11, 3 and 0.14, and keeps about 2 MB of tree state per vault
  at 5000 devices. mls-rs has had no third-party audit.
- MLS's gain over wrapped keys is post-compromise security for a stream
  of messages; a vault is data at rest whose old keys a member keeps
  anyway.
- `hpke` 0.15 has had no paid audit and pulls in post-quantum KEMs by
  default. The same sealed box from `curve25519-dalek`, `hkdf` and
  `chacha20poly1305`, all in the tree, is how the history already seals
  entries to a key (`history/pending.rs`).

## Decision

`tauminus-team` holds the protocol, with no I/O: the client verifies
everything in it, and the Hub stores and forwards its bytes.

### Devices and trust

Each device has an Ed25519 key that signs and an X25519 key that keys
are sealed to; its card names both and its user, and its fingerprint is
SHA-256 over the card. The Hub never vouches for a key. A key counts
only once a log entry an admin's device signed adds it, and the admin
adds it after comparing fingerprints out of band. The invite carries
the hash of the vault's first entry and of the newest entry the admin
held; a joining device refuses a log that holds either one differently,
so a removed admin cannot hand it a log that branches off before the
removal.
OIDC signs a device in to the Hub and says nothing about its keys:
the Hub receives those tokens, and an identity provider that is broken
into would otherwise add keys.

### The membership log

Each vault has an append-only log. An entry names its sequence number,
the hash of the entry before it, the device that signed it, and one
operation: create the vault, add a member, add a device, change a
member's role or credentials, or remove a member or a device. The
creating entry is signed by its creator; every later one by a device of
an admin at that point. A client verifies the chain from the first
entry and refuses an entry that differs from one it holds at the same
position; entries it already holds, sent again, change nothing. At
least one admin keeps a device in the vault. A device enters a vault
once: after its removal it is never added back, and a member invited
again comes with new device keys.

### Keys in epochs

Each epoch has two random 256-bit keys, never derived from earlier
ones: a data key for hosts, snippets and folders, and a secrets key for
passwords and private keys. The data key is sealed to every device, the
secrets key only to the devices of members granted credentials. A seal
is an ephemeral X25519 key, HKDF-SHA256 over the shared secret, and
XChaCha20-Poly1305, with the vault, epoch, device and key kind bound in
as associated data.

Removing a member or a device, or taking a member's credentials, starts
a new epoch in the same entry: fresh keys sealed only to who remains,
and the previous epoch's keys sealed under the new ones, so a device
holding the current keys walks back to every earlier epoch. A new
member gets the current keys and reads the vault's history the same
way. Old items keep their epoch: the removed member held those keys, so
sealing them again protects nothing. A member granted credentials again
after they were taken reads the secrets of the epochs in between too.

### Items

An item is sealed under its epoch's key with the vault, item id, epoch,
the log sequence and hash it was written against, the writing device
and a per-device counter as associated data, and the writing device
signs the whole. A reader accepts it only when that log position is in
its own log, the epoch is the one current there, and the writer's
member held edit or admin there; otherwise the item is refused, so a
view-only member cannot plant a host. A device writes only against the
newest log it holds, and the Hub refuses a write against an older one.

The writer names the log position, so a member who lost the right to
write could name one from before. The entry that takes the right away
(removing a member or a device, a role below edit, or credentials for a
secret) therefore lists the SHA-256 of each version that member's
devices wrote and the admin's device holds, and a reader accepts an
item from a writer who may not write at the newest entry only when an
entry after the item's position lists that very version. A version the
admin's device never received is lost with the right to write.

Which of two valid versions of an item is newer is the sync layer's to
say, from the stamps sealed inside each record (ADR 0030); the counter
in the header only keeps two writes of one device apart.

### What a revoke covers

A removed member keeps every key and item it held. The app lists the
credentials in vaults that member could read, so they are changed on
the servers themselves; nothing else takes back a password already
read.

## Consequences

- The Hub sees members, devices, vault ids, item counts and sizes,
  write times, which device fetched what, and addresses. It cannot read
  names, hosts, snippets or credentials, add a key, or change a role.
- A Hub that colludes with a removed member and hides the removal from
  a writer who never syncs with another member can still have that
  writer seal under the old epoch. A removed admin can likewise sign
  entries off the log as they last held it, which a device that has not
  yet seen the removal accepts. The Hub's own refusal of stale writes,
  and members' clients comparing the newest entry they hold, narrow
  both; nothing in the client alone closes them.
- A view-only member can still read what it can see; a member with
  credentials can connect with them, so "use without seeing" is a
  limit of the app, never of the cryptography.
- Removing a device seals one or two keys to every remaining device, 133
  bytes each, plus 32 bytes for each version kept, which one entry lists
  once. Measured on a release build at 1000 members with 5 devices each:
  a removal's entry is 1 MB and takes 0.73 s to build; applying it takes
  17 ms, at the first epoch as at the 200th. The log keeps each device's
  newest wrap of each kind, 133 bytes apiece, so a keyring opens from
  the log alone in O(epochs). Each later sync copies the membership
  state, 1.2 ms there, and verifies what is new.
- A device that joins downloads every wrap ever sealed: after 200
  removals at that size, about 200 MB, verified in 3.4 s. The Hub serving
  each device only its own wraps, against a commitment to the list in
  the entry, or admin-signed checkpoints, is the next step when vaults
  reach that size.
- Opening an item checks its Ed25519 signature, 48 of its 66
  microseconds: 6.6 s for 100 000 items. The Hub client checks each
  version's signature once and keeps that result by its digest. Whether
  its writer may still write depends on the log, so that check runs on
  every opening: a device behind the log can accept a version that an
  entry it has yet to see takes away.
- Kept versions grow with every right taken away; the Hub client can
  keep only the newest version of each item once the sync layer says
  which that is.
- This is a protocol of the project's own, so the Hub's security review
  (a v0.6 exit criterion) covers it with this ADR as its specification.
- The tests for the exit criteria run on the protocol first, and again
  through the Hub once it serves it.
