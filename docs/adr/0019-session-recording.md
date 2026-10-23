# ADR 0019: Session recording and replay

- Status: accepted
- Date: 2026-10-23

## Context

v0.4 records sessions to asciicast v2 and plays them back with seek,
search, bookmarks, and comments. The exit criterion: a recording of a
one-hour session seeks under 200 ms. A recording holds whatever the
screen showed, secrets printed by a command included, so it must be kept
as carefully as the command history.

## Decisions

### Recorded by the backend, sealed to the vault

The session pump hands each batch it sends the view to a recorder too,
along with resizes, so a recording holds exactly what the pane got. The
recorder turns batches into asciicast lines (a character cut between two
reads is held until it is whole; bytes that are not UTF-8 are dropped, as
xterm.js drops them) and passes them to a writer thread over a bounded
queue, which seals 64 KiB at a time and flushes within a second of a
quiet spell. The tap goes on before the file is made, so the gap between
the screen the view sent and the first recorded byte is one IPC call. The
pump never waits on the disk: when the writer falls 1024 lines or 16 MiB
behind, when a write fails, or when a recording reaches 512 MiB as
written (what the player can load in one piece), the recording stops
there and is marked cut short. Only output is recorded, never typing, so a
password typed at a prompt that does not echo stays out.

A recording is a sealed stream: frames of XChaCha20-Poly1305 under a key
from X25519 with a recordings key pair the vault holds, HKDF-SHA256, and
each frame's index, its file's id, its kind, and whether it is the last in
its additional data. Writing needs only the public key, which the vault
file keeps readable while locked (bound by its manifest, as the history's
is), so recording works with the vault locked; playing back needs it
open. While the vault is open, recordings are sealed to the key it
checked at unlock, not to the one read from the file. The key pair is made the first time a recording starts, which needs
the vault open once, and outlives password changes, since every recording
is sealed to it. The list (title, host, time, length, size) is kept in the
clear beside each recording, so it shows without an unlock. Files are this
user's only.

A recording started in the middle of a session begins with the screen as
it was, serialized, as its first event, so it plays back from what was
there, in Tauminus or any asciicast player. The screen only: scrollback
from before the user asked to record stays out.

A recording holds what the host sent, unfiltered, so the guards the live
pane puts on output (a REP sequence asking for gigabytes, among them)
apply to every terminal that plays one back too. Links in a recording open
nothing. What is read back from the store (checkpoints, bookmarks) is
bounded and checked as input: sealing says who can read a file, not who
wrote it.

### Seek through checkpoints built from the recording

Two panelists measured the options against a 200 MB hour (two million
events). Replaying from the start costs seconds. `@xterm/addon-serialize`
alone restores a screen that then plays back wrong in 65% of seeks inside
full-screen programs: it leaves out the scroll region, character sets, tab
stops, the saved cursor, the cursor's exact place, a hidden cursor, and
the current attributes, and a checkpoint cut inside an escape sequence
corrupts what follows. So:

- A snapshot is the addon's output plus a suffix that restores those,
  read from xterm.js 6.0.0's internals (pinned, and held to account by a
  differential test that writes the same continuation into a restored
  terminal and the original), taken only between escape sequences.
- A worker builds checkpoints by playing the recording into a headless
  xterm.js once, one every 20 ms of parse time (at most every MiB), with
  200 lines of scrollback. Parse time, not bytes, bounds a seek: a burst
  of empty lines parses 20 times slower per byte than text. A checkpoint
  also waits for four times the previous one's size in output, so all of
  them weigh at most a quarter of the recording plus one snapshot: a
  snapshot costs scrollback × columns × the colour in each cell, and
  truecolour in every cell made 1000-line checkpoints 5 times the size of
  the output they covered.
- A seek restores the nearest checkpoint into a scratch terminal in the
  worker, replays to the target, and sends one snapshot; the page writes
  it into a hidden terminal and shows it, one paint instead of many.
- The checkpoints are saved, sealed like the recording, once the worker
  is idle, and used on the next open if their format (xterm.js and Unicode
  versions, scrollback) matches. Below 3 MB of output none are built: a
  replay from the start is fast enough.

Checkpoints are built from the recording rather than while recording:
built from the same bytes, they always agree with it, where ones taken by
the live pane would have to line up byte for byte with what the backend
wrote.

Measured end to end in the e2e test (40 MB of output over an hour): seeks
of 29 to 76 ms on a loaded machine, the first one included.

Search scans every event (O(n), about 4 s for 1.7 million events) in
8 ms slices: seeks run between them, and a newer query ends an older one.

### Bookmarks and comments

Bookmarks (a time, a label, a comment) are sealed like the recording and
replaced whole on each change; at most 1000, each under 4 KiB. Changes
wait for the list to load and go one at a time, each from the list the
one before it saved.

## Consequences

- A recording follows its session: a reconnect starts a new session, and
  recording it again is the user's choice.
- An hour of very heavy output (hundreds of MB) takes the worker tens of
  seconds to index the first time; seeks before the index reaches the
  target replay further and are slower until then.
