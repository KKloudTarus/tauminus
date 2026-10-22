# ADR 0008: Command history in SQLCipher, sealed while the vault is locked

- Status: accepted
- Date: 2026-10-22

## Context

Users want to find a command they ran last week, on any host, by a word
in the command or in what it printed. That text often holds secrets:
tokens in arguments, passwords pasted into output. So the history must be
encrypted at rest, kept out of logs, and never hold what was typed at a
password prompt. Commands must be recorded while the vault is locked too,
since it locks itself after 15 minutes idle. A search over a million lines
must answer in under 100 ms.

## Decisions

### SQLite with FTS5, encrypted by SQLCipher

`history.db` is SQLite with an FTS5 index over command and output,
encrypted page by page with SQLCipher (rusqlite, built with a vendored
OpenSSL so every platform runs the same cipher code). The write-ahead log
is encrypted too. Deleted rows are overwritten (`secure_delete`), and
their words leave the index at once (FTS5 `secure-delete`); "Forget
history" also merges the index. The tokenizer folds diacritics, and `đ`
to `d` as well, so "dang nhap" finds "đăng nhập". A hidden index column
holds one token per host and per exit status, so those filters intersect
posting lists. Each row keeps when it was stored, which never decreases
with the rowid; a command is stored after it starts, so a start time
becomes a lower rowid bound. Prefix indexes of two to eight letters keep
every keystroke of a search cheap: a longer last word matches on its
first eight letters, a one-letter last word is matched as a word, and a
search reads at most eight words. A trigger-kept byte total makes a prune
under the cap O(1).

Searches read on a connection of their own, so with the write-ahead log
they never wait for a write. Writes share one fair lock: a prune deletes
in chunks sized to take about 40 ms, and a record waiting for the lock
gets it next.
A history file the key does not open, from an earlier vault, is renamed
aside and a new one started. The key is raw: the vault's Argon2id already
guards it, so SQLCipher runs no KDF of its own.

A blind index (keyed hashes of tokens in a plain database) was the
alternative. It leaks how often each token occurs and cannot do prefix
search; SQLCipher leaks file size only.

Measured on a million lines (100,000 commands, ten output lines each),
committed ten commands at a time as recording does, release build: every
one of thirteen queries, filters and eight words included, has a 95th
percentile under 3 ms, fifty results with 64 KiB outputs under 25 ms,
and a prune under the cap takes 0.2 ms. While 500 commands are pruned
(1.4 s), a record waits under 400 ms; while one host of 20,000 commands
is forgotten (5 s), a record waits up to 3 s, and a search never waits
more than 30 ms. The excerpt under each result comes from the first
16 KiB of its output, where a word matches in any ASCII case; a word
found only by folding its diacritics shows no excerpt.
CI runs this benchmark on Linux and fails over 100 ms.

### The keys live in the vault; locked, commands are sealed

Two panelists argued for the OS keychain and for sealing; the synthesis
chose sealing. A keychain key is as strong as the OS login, which is
weaker than the vault for anyone who turned the vault's keychain off, and
Linux without a Secret Service has none. Keeping entries in memory while
locked loses them on exit.

The vault holds the database key and an X25519 key pair. The public key
sits in `vault.json`'s header, readable without unlocking and bound into
the vault's manifest, so an edited key fails the next unlock. While the
vault is locked, each command is sealed to it (ephemeral X25519,
HKDF-SHA256, XChaCha20-Poly1305) and appended to `history.pending`, which
a locked app can write and not read. The next unlock opens the database,
imports the pending entries, and only then removes the frames it read,
under one exclusive lock on the file, so two windows unlocking at once
import each entry once. Recording and
importing share one lock, so nothing sealed during an import is lost. A
crash mid-append leaves a short last frame; the next append cuts it off
first, so nothing written after it is lost. Every operation on the file
holds an exclusive lock on it. The pending file holds at most 16 MiB,
which imports in a couple of seconds.

Searching needs the vault open. Every use checks the vault's state as it
is now, its idle time included, so nothing reaches the database after a
lock, or after the vault has gone unused past its idle time, even before
the history closes. A database that will not open keeps commands sealed.

Same-user writers are out of scope, as in ADR 0007: whoever can replace
the public key can also set a `ProxyCommand`.

### What is recorded

The view hears what reaches the shell: behind the prod guard, only what
the guard let through, so a refused command is never recorded. On Enter
it reads the command from the screen, as the prod guard does (ADR 0006):
from where the line editor started at this prompt, which the first key of
any kind marks (a letter, Up, Ctrl+R; not a terminal's own reply), or
else what follows the last prompt-like ending on the line. A line is a
command only after a shell's prompt (ending in `$`, `#`, `%`, `>`, or a
common theme's `❯`) that ends as the last command's prompt did, and, for
`>`, starts as it did too; a bare `>` is the shell's PS2 or a REPL's. Anything typed into a running program,
an answer, stdin, a password, a line at `db:6379>`, is not a command, and
is cut from that program's output with every row it wrapped onto, which
keeps the program's prompt only. It follows bash's reverse search, joins
continuation lines (`> `) into one command, takes a pasted command with
lines from the paste, and splits a paste that arrived without brackets at
its line endings. A right prompt is left out. Its output is the text
between its line and the next prompt, at most 2,000 rows; the backend
keeps 4 KiB of command, with newlines as spaces, and 64 KiB of output.

With shell integration (OSC 133), the shell's own marks say where the
prompt, the command, and the output start, and how the command ended;
they replace the guesses, and only a line typed between its B and C marks
is a command; a line typed after Enter and before C goes on with it.

Nothing is recorded at a prompt for a secret (password, passphrase, code,
API key, in several languages), when the keys typed do not show on the
screen (the remote echo is off), when the line starts with a space (bash's
`ignorespace`), in a full-screen program, or at a REPL's prompt. A secret
typed inside a running command, such as at sudo's prompt, is cut from that
command's output, which keeps the prompt only. Secrets of known shapes
are hidden in commands and output whatever prompt they came after: AWS,
GitHub, GitLab, Slack, OpenAI, Stripe, Google, npm, Vault, and Docker
tokens; bearer tokens and JWTs; private key blocks; the value of
`NAME=value` and `--name value` when the name holds PASS, SECRET, TOKEN,
KEY, or CREDENTIAL; `-u user:password`; a URL's password; and a MySQL
client's `-p<password>`. A saved host or group can
turn recording off; the nearest level wins, and the root default is on.
The backend checks the host's setting, not the view. A session opened
from another one (`ssh prod` typed in a local tab) records under the tab
it runs in.

### Retention

The oldest commands go first: anything over a year old, then as many as it
takes to keep the text under 256 MiB. Pruning runs in the background on each
unlock and every 500 commands. "Forget history" clears one host or everything. Forgetting one host
deletes its rows in chunks, then merges the whole index, which drops
their words; the merge holds the writer for time proportional to the
index, seconds on a large history, and records wait for it.

## Consequences

- No vault, no history: there is nowhere to keep the keys.
- A command typed faster than the remote echo, on a slow link, may be
  recorded short or not at all; the recorder prefers missing a command to
  recording a secret.
- A command typed while the one before still runs (type-ahead) is taken
  for input to that command and not recorded, and output that ends
  without a newline is cut at the next prompt's row.
- The file is about four times the text it holds, index included: about
  1 GiB at the 256 MiB cap.
- Without shell integration, a prompt ending in `>` that changes its
  start (fish's working directory) misses the commands typed at it;
  fish 4 sends the marks.
- Timestamps are when a command started, kept within the last day of the
  backend's clock.
- Replacing `history.db` with an older copy is not detected.
