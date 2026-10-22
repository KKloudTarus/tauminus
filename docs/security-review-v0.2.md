# Security review: vault and history storage, v0.2

- Date: 2026-10-22
- Scope: `vault.json` and the keychain slot (ADR 0007), `history.db`,
  `history.pending`, and the recorder (ADR 0008), the presence check
  (ADR 0009), and every path a secret takes through IPC, memory, and logs.
- Code: `main` at `fa1055d`, with the fixes below and their own review.

## Method

Each change had a gate review while it was built: QA, security, and
performance agents, four rounds for the history. This review read the
merged code as a whole, with two independent security reviewers:

1. Cryptography and file formats: nonces, additional data, key
   separation, KDF bounds, parsing of hostile files, what the files show
   without the keys.
2. The lifecycle of secrets: what crosses IPC, how long keys and records
   stay in memory, logs and error text, what the recorder captures, the
   keychain and the presence check.

Both ran the test suite and wrote probes against the real crates. The
findings below were checked against the code before they were fixed.

## Findings

| Severity | Finding | Status |
|---|---|---|
| Medium | A password change kept the history's keys: an old `vault.json` and the old password opened every command recorded afterwards. | Fixed. The history moves to new keys (ADR 0008); `tests/password_change.rs` reproduces the attack and fails without the fix. |
| Medium | Redaction missed common shapes: `name = value`, JSON and XML fields, openssl `pass:`, `redis-cli -a`, `ssh-keygen -P`, `smbclient -U u%p`, the `token` scheme, short bearer tokens, PuTTY keys. ADR 0008 claimed more than the code did. | Fixed, each shape in the tests; ADR 0008 lists exactly what is hidden. |
| Low | One impossible frame length in `history.pending` made the next append cut every frame after it. | Fixed: the file is set aside whole. |
| Low | SQLCipher's settings were not pinned; a later default would have set the history aside. | Fixed: `cipher_compatibility = 4`. |
| Low | A password change kept the file's Argon2id costs, however low. | Fixed: raised to at least the defaults. |
| Low | A background prune kept its keyed connection open after a lock. | Fixed: it stops between chunks once the vault locks. |
| Low | Offline, the pending file shows how many commands were recorded while locked and their lengths; `vault.json` shows record ids and lengths. | Documented in ADR 0008. |
| Low | A same-user process can seal commands of its own into the pending file. | Accepted with the same-user exclusion, documented in ADR 0008. |
| Low | Keys and records can reach swap or a crash dump; SQLCipher cannot lock its key pages under the default memory-lock limit; a connect keeps credentials it took before a lock. | Documented in ADR 0007. |
| Low | `history_record` trusts the host id the view sends for the session. | Documented in ADR 0008; a compromised view can already do more. |
| Low | The SQLCipher key text passed through SQLite's statement buffers, which it frees without wiping. | Fixed: the key goes through `sqlite3_key_v2` and `sqlite3_rekey_v2` from a wiped buffer; a test opens a file keyed the old way. |

No finding was critical or high.

Moving the key into `sqlite3_key_v2` had its own gate review, which found
that SQLCipher answers OK even for a re-encryption it rolled back partway
(a full disk): the history then took the file, still under the old key,
for another vault's and set it aside. The file is now checked after the
re-encryption, and a file still under its old key is reported, kept, and
moved again later; a key call that fails is no longer taken for a wrong
key.

The fixes had their own gate review, which found five more, all fixed:

| Severity | Finding | Fix |
|---|---|---|
| High | A failed re-encryption (a full disk) was retried, in full, on every history use. | One try per unlock; commands are sealed meanwhile. |
| Medium | Two password changes before the history moved lost the key the database was under, and set the history aside. | Every unconfirmed key generation is kept; `tests/password_change.rs` covers two changes and a command sealed between them. |
| Medium | The XML pass could split a multibyte character and panic, so server output could stop a command being recorded. | The cut falls on a character boundary; a test covers it. |
| Medium | New rules hid ordinary text: `openssl -keyout`, JSON and XML fields named `Key`, `token_type`, `public_key`. | Names are read by parts, `KEY` needs a word saying whose, and attached values apply to `-p` of the MySQL clients only; the tests list the text that must stay. |
| Low | The re-encryption did not ensure no other connection had the file open; a damaged pending file lost the whole frames before the damage. | It runs under a rollback journal, which refuses while the file is open; the whole frames move to the new file. |

A second gate round found two more ways to lose the history, both fixed:

| Severity | Finding | Fix |
|---|---|---|
| Medium | A second window still holding the vault from before a password change took the moved history for another vault's and set it aside. | The history compares the file's public key with the window's first; a mismatch moves and sets aside nothing. A two-window test covers it. |
| Medium | A password change during a long re-encryption had its new generation dropped when the open finished. | Only the generations that open used are dropped; a test covers it. |
| Low | A busy re-encryption blocked the history until the next unlock; an append waiting for the pending file's lock could write into a file just set aside. | Busy is retried on the next use; an append checks it holds the file the path names. |
| Medium | Narrowing `KEY` let `APP_KEY`, `SSH_KEY`, and `HMAC_KEY` through, and camelCase names (`SecretAccessKey`, `secretKey`) were never split. | Names split at camelCase too; a last part `KEY` is a secret's in assignments and flags, and needs a word saying whose only in JSON and XML. |
| Low | A nested JSON value under a secret's name hid its first field and kept the rest; `.netrc` passwords; `docker run -p` and `ssh -p` ports hidden as passwords. | Nested values are judged field by field; `.netrc` lines are read; `-p` is a password only after `login` for docker, podman, and az. |

## Verified

- Every seal takes a fresh 24-byte nonce from the OS random source; each
  pending frame has its own ephemeral X25519 key; low-order points are
  refused.
- Additional data binds the vault id, format, slot kind, KDF costs and
  salt, record ids, and the history public key; deleting, reordering,
  mixing versions of records, or relabelling a slot fails to open.
- KDF costs out of bounds are refused on every unlock path.
- Hostile files are read with bounds: 16 MiB per file, 8 slots, 4096
  records, 256 KiB per pending frame.
- Nothing secret crosses IPC outward; secrets that come in become wiped
  buffers at once; `Debug` hides them; no log line carries one.
- Files are 0600, written atomically under a lock; the history database
  overwrites deleted rows and their index words.
- The presence check runs in the backend, asks afresh every time, and
  fails closed.

## Accepted residual risks

- Same-user processes: they can read the keychain entry, replace the
  history's public key, forge pending entries, or put back an older copy
  of a file.
- The keychain slot is as strong as the OS login; the presence check
  guards the app's own unlock only.
- A secret in a shape the redaction does not know, such as a password
  typed at a shell prompt by mistake, is recorded.
- Decrypted data in ordinary memory while the vault is open.

## The v0.2 exit criteria, checked

| Criterion | Evidence |
|---|---|
| Prod guard rule tests: 200+ commands, no false negative, false positive rate recorded | `crates/tauminus-domain/tests/guard_corpus.rs` reads 322 dangerous and 194 safe lines; every dangerous line is flagged with its rule; 1 safe line of 194 asks (`echo "DROP TABLE is dangerous"`), printed by the test. |
| Wrapped and obfuscated forms caught | The dangerous corpus holds `sudo` and `env` forms, `bash`, `sh`, and `zsh -c` (11 lines), `$(...)` (11), backticks (4), and `;`, `&&`, `\|\|`, `\|` chains. |
| Vault tests: round trip, wrong password, tampered ciphertext, truncated file | `crates/tauminus-store/src/vault/tests.rs`: `records_round_trip_through_the_file`, `a_wrong_password_is_refused`, `a_tampered_file_is_refused`, `a_truncated_file_is_refused`. |
| Secrets never appear in logs, crash reports, or history | `crates/tauminus-store/tests/no_secrets.rs` runs the vault and the history end to end with every log line captured at TRACE, scans every file and log line for each secret and for the history keys in hex, and reads the history back decrypted to check the password in a recorded command was hidden. Tauminus writes no crash reports. |
| History search over a million lines under 100 ms, in CI | `a_million_lines_are_searched_in_under_100_ms`, run by `.github/workflows/ci.yml` on Linux, fails over 100 ms. Of its 15 query shapes, 13 have a p95 under 2 ms; the slowest, 50 results with outputs of about 40 KiB and 8 words, took 21 to 44 ms at the worst of 10 runs on this machine. |
| Security review of vault and history storage | This record. |

The presence check (ADR 0009) runs the OS's own calls in CI on macOS and
Windows runners, and prints what each answered; where the check is
unavailable, the test asserts it refuses an unlock. A prompt answered with Touch ID
or Windows Hello has not been run on real hardware; the polkit path was
run against a real polkitd and agent.