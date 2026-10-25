# ADR 0031: trzsz transfers

- Status: accepted
- Date: 2026-10-25

## Context

ADR 0026 left trzsz for later: no Rust crate implements it, and the npm
library would put file bytes through the webview. `trz` and `tsz` are
what many people use instead of `rz` and `sz`, because they work inside
tmux on the host and print nothing that looks like line noise.

The protocol was read from trzsz-go v1.2.0 (commit `b1eaca3`) and the
Python trzsz 1.1.7 (`f73b234`). A host prints
`ESC[s::TRZSZ:TRANSFER:<S|R|D>:<x.y.z>:<13-digit id>:<port>` and CR LF,
switches its terminal to raw mode, and waits for the terminal's `#ACT:`
line with no timeout. Messages are lines `#TYPE:payload` and LF. Numbers
are decimal; text is base64 of zlib. Protocol 1 is stop and wait; 2 to
4 add pipelining with zstd, resume, and folders as archives.

The reference clients check no name the host sends: `..` in a name, or
in a folder path, writes outside the folder the user chose. The host
decides whether the client overwrites files, and the mode bits of what
it creates. Lengths, counts and sizes are trusted as sent.

A panel of three (security, architecture, and an advocate for the
smallest scope) weighed the version, the modes, where the code sits, and
the limits.

## Decisions

### Protocol 1, base64, files only

The terminal answers with protocol 1, `binary:false`, `support_dir:false`,
no tunnel, no fork, no tmux control mode. Every server in use runs its
protocol 1 path for that: Python takes `min(1, 1)`, trzsz-go 1.1.0 to
1.1.3 echo the number and run protocol 1 unless it is 2, and 1.1.4 on
take the minimum. A server started with `-b` falls back to base64 when
the client says `binary:false`; trzsz-go assumes binary when the key is
missing, so it is always sent.

Base64 keeps ADR 0026's rule that nothing sent can run as a line: an
upload line holds `#DATA:` and `[A-Za-z0-9+/=]` only, which bash and
fish read as a comment and zsh as an unknown command, if `trz` dies and
the shell reads it. Binary mode leaves line ends raw. Protocols 2 to 4
need zstd, which is not in the tree, and bring JSON names with mode
bits and folder paths, resume, and archives: most of the reference
clients' unchecked input. The panel estimated stop and wait at 96 to 98%
of a 20 ms link and 80 to 89% at 100 ms, before base64's third.

`trz -d` (mode `D`) and `tsz -d` are refused: the first in its banner,
the second by the server, which says the client does not take folders.
A Windows host (an id ending in `10`, or `1`) needs another line reader
and is refused in its banner. Protocol 2 and folders get their own ADR
when users ask.

### The trigger, held as Zmodem's offer is

The pump looks for trzsz in the same pass as `find_offer`, before the
view, the recent ring, the recording and triggers see output, and only
in sessions that can transfer (not tmux control mode or Mosh tabs). One
domain search returns the first of the two kinds; the tail it keeps
across reads covers the longest trigger, so one cut between two reads is
still found.

A trigger counts only whole: `::TRZSZ:TRANSFER:`, the mode, a version,
the 13-digit id, an optional port, and CR LF, in raw bytes. A line from
tmux control mode (`%output`) carrying one is shown, never answered. An
id seen before in the session is shown, never answered: `cat` of a log
or a replayed recording repeats ids. Output is held from `::TRZSZ`; the
`ESC[s` before it reaches the view, since the server's last message
restores that cursor and clears below it.

The gate is ADR 0025's: output held up to 256 KiB, a numbered offer,
400 ms before the buttons act, keys refused, Ctrl+C declines, and 10 s
in which a new offer goes unanswered. Nothing is written to the host
until the user accepts; for an upload, the backend's file dialog (ADR
0026) comes first.

A decline the user makes, or Ctrl+C, writes one `0x03`: a waiting `trz`
or `tsz` reads it as an interrupt, prints `Interrupted` and exits, and
anything else that printed the trigger gets what Ctrl+C would send. A
decline the pump makes (too much held, or within the 10 s) writes
nothing, so text never goes into a program that only printed the
trigger.

### What the host's configuration may set

After `#ACT:`, the host's `#CFG:` must come within 20 s, or the offer
ends with nothing more written. It is a flat JSON object; the domain
reads it with a small reader of its own (it takes no serde), bounded to
64 KiB of JSON and 32 levels. The transfer stops before any file if it
asks for binary, folders, a newline other than LF, or a protocol other
than 0 or 1, or a `bufsize` past 10 MiB. `overwrite`, `quiet`,
`escape_chars` and mode bits are ignored. `timeout` is held to 5 to
60 s, and 0 or less means 60.

tmux on the host (`tmux_output_junk`) is taken: lines tmux wraps are
joined, its status sequences dropped, and a line is read from the last
`#` and the type expected. The pane's output during a transfer comes
from `trz` alone, in raw mode, so another frame in the same line needs a
second writer to that terminal, which already holds the session.

### Names and files

A name from `tsz` goes through the steps Zmodem's do, moved into one
domain function both call: its last path component, a leading `.` made
`_`, `local_name` (no `..`, separators, control or reordering
characters, Windows device names, trailing dots or spaces, or more than
255 bytes), and no invisible characters. Files are written through
`ReceiveFolder`: created new under a part name, never over a file and
never through a link, moved to their name, a taken name getting a
number. The file becomes its name only once the host's MD5 matches; a
mismatch removes it. `#SUCC` tells the host the name only, and the
`#EXIT:` text names no local folder, which would tell the host the
user's name.

### Limits

A line is at most 16 MiB for `#DATA` (a 10 MiB chunk, zlib and base64),
64 KiB for any other. Each chunk is inflated in a stream that stops past
what the file still has to bring, at most 10 MiB, so a zlib bomb stops
early. Numbers are at most 20 ASCII digits; `#NUM` is at most 100 files.
The line reader resumes from where it stopped scanning, so a long line
in small reads costs O(n), not O(n²). Each download holds at most about
30 MiB.

An upload starts with 64 KiB chunks and doubles them while a chunk is
acknowledged within 500 ms, up to the host's `bufsize` or 1 MiB. Chunks
go out through ADR 0026's queue, at most 1 KiB per write, read while
fewer than 128 KiB wait, with epochs so a stop drops what has not left.

### Stopping

A stop raises the epoch, then writes `0x03`, which a live `trz` or
`tsz` reads as an interrupt wherever a line is, then Ctrl+E and Ctrl+U,
as ADR 0026 does. Output is then dropped until the server's last message
(`ESC[u ESC[0J` or `ESC 8 ESC[0J`), 300 ms of quiet, or 30 s, so the rest
of a `#DATA` line never reaches the view or the recording. A line from
the host that is not the protocol's, while files go, counts as no
progress; 64 KiB of it ends the send, as Zmodem's does. A `#fail` or
`#FAIL` message is shown as text, control and reordering characters
dropped and cut to 512 bytes: trzsz-go puts a stack trace in `#FAIL`.

### Where the code sits

`tauminus-domain` gets a sans-IO `trzsz` module beside `zmodem`: the
trigger search, the line reader, the base64 and zlib codec, the `#CFG`
reader, and the download and upload state machines, which speak the
same `Received`, `Need` and `Sent` steps the pump already handles. It
takes three pure codecs, allowed in the layer rules as `zmodem2` is:
`flate2` (Rust backend), `data-encoding` and `md5`, all already in
`Cargo.lock`. The pump's `Receiving` and `Sending` hold an engine, Zmodem
or trzsz, so the state machine, progress, lists and draining stay one;
the decline bytes and the end of a drain become the engine's. A poll may
now say it wrote and wants another, so one long line never passes the
queue's bound at once. The offer carries its protocol to the view, which
changes the banner's words; the commands, dialog and file types stay.

## Consequences

- Sessions with no transfer: one more SIMD search per chunk, for
  `::TRZSZ`; no measurable cost beside the Zmodem one.
- Download: O(n) per byte (line scan, base64, inflate, MD5, write);
  about 30 MiB at most per transfer. Upload: O(n) per byte, at most
  1 MiB of chunk and 128 KiB queued.
- Folders, Windows hosts, binary mode, resume and protocols 2 to 4 are
  not supported; each is refused with a reason the user sees.
- Live tests run `trz` and `tsz` from pip trzsz 1.1.7 and trzsz-go
  1.2.0 on a scratch sshd, with and without tmux on the host, and kill
  `trz` mid-upload under bash and zsh to check no line runs.
