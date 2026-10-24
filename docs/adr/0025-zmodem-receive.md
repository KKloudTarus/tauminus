# ADR 0025: Files a host sends with Zmodem

- Status: accepted
- Date: 2026-10-24

## Context

v0.4 lists Zmodem and trzsz transfers. Running `sz FILE` on a host sends
the file over the terminal itself, with no SFTP subsystem and through
any jump host. SecureCRT and Xshell have it; Termius does not. A panel of
two (architecture, security) and a spike against lrzsz 0.12.21 weighed
where the protocol runs, which sessions carry it, and what a host may do
before the user agrees.

Tabby's CVE-2026-45036 is the case to avoid: its Zmodem code answered a
host's offer on its own, so output that only looked like an offer (`cat`
of a file, a crafted MOTD) started a transfer that wrote where the host
chose.

This first slice receives (`sz` on the host). Sending with `rz`, and
trzsz, come after it.

## Decisions

### The protocol runs in the backend's output pump

`tauminus_domain::zmodem` wraps `zmodem2` 0.7.3 (MIT OR Apache-2.0,
`no_std`, sans I/O). It finds the offer, a ZRQINIT hex header whose flags
and CRC are all hex digits, with `memchr`'s SIMD substring search (about
25 GB/s; a byte-wise scan ran at 0.9 GB/s and cost a flood 30 to 41 µs
per 32 KiB chunk). A 17-byte tail finds a header cut across reads. It
drives a `zmodem2::Receiver`. The session pump looks
at output before anything else does: the view, the recent ring that
password fill and the AI read, the recording tap, and the trigger and
highlight batches. File bytes never reach any of them.

The spike moved 3 MB from lrzsz's `sz` in 203 ms; over a real SSH shell
with a PTY the app receives the same 3 MB in 0.66 to 0.79 s, with the
host's own `sha256sum` matching.

### Only sessions that pass bytes through

`SessionMeta::transfers` is true for SSH sessions on a UTF-8 host, whose
bytes go from the channel to the pump untouched. Mosh draws a screen,
tmux tabs carry `%output` text, Telnet escapes IAC, and a host with
another character set is decoded on the way; none of them carry the
protocol, so their output is never looked at for an offer.

### Nothing goes back before the user agrees

When an offer is found, the pump shows output up to it, holds the rest
(at most 256 KiB), and asks. Nothing is written to the host until the
user accepts, and the buttons wait 400 ms after an offer appears, so a
click meant for what was there before does not answer it. Each offer
has a number, and an answer or a cancel names
the offer it was given for: a second click on Receive, a click that
lands after the offer went, or an answer given ahead for an offer still
to come, can never accept a later offer. Declining shows the held output as it came, so `cat` of a
file that holds an offer header prints whole. For 10 s after a decline,
offers are not asked again: `sz` repeats its header.

While an offer waits or files come, `SessionManager` refuses every key
with `Transferring`: keys would go into the protocol. A write with
Ctrl+C in it cancels instead. The view drops keys the same way and shows
a banner with Receive, Decline, Stop, and the result.

### Files land in `Downloads/Tauminus`, under names the host cannot steer

A name the host sends is cut to its last path component, a leading `.`
becomes `_` (no hidden dotfiles), and it must pass `local_name`, the
same check SFTP downloads use: no separators, control or reordering
characters, Windows device names, or the `.tauminus-part` suffix. A name
that fails, or that holds characters which read as nothing (U+3164,
U+200B and the like, which could hide `.exe` after `.pdf`), is skipped,
and shown with what hides or reorders text taken out. A file is written as `NAME.tauminus-part`, created new and `0600`,
and linked to its name only when complete; a taken name gets ` (1)`,
` (2)` and on (the next number is remembered per name), a stem too long
for 255 bytes with the suffix is cut, and nothing is replaced. A file
larger than the size the host declared aborts the transfer; an empty
size field means no size. On Windows a received file gets a
`Zone.Identifier` stream (zone 3), as a browser download does. A part
file is removed when the session ends before it is complete.

### A stop leaves the shell usable

Cancel, an idle host (5 s timeouts, 10 in a row), a write error, data
the protocol cannot read (a size past 4 GiB among it), more than 256 KiB
of replies the host has not read, or the host's own abort sends the
cancel sequence and drops the part file. The file data the host had
already sent still comes, up to the SSH window. The pump drops it until
the user has typed (Ctrl+C aside, which users press again and again)
and a chunk reads as text (UTF-8, no ZDLE, only the control characters
a shell prints), the host is quiet for 300 ms, or 30 s pass. What the
shell printed as `sz` exited goes with it; the banner says Enter brings
the prompt back. A file's text read with no ZDLE in it still shows if
the user types something other than Ctrl+C just before it comes. A session closing mid-transfer shows none of it.
After a complete transfer only `sz`'s closing `OO` and line ends are
dropped.

## Consequences

- Cost when no transfer runs: one SIMD scan of each output chunk, O(n)
  at about 25 GB/s, plus a 17-byte tail copy per chunk.
- During a transfer: O(n) per chunk; `zmodem2` decodes at 70 to 90 MB/s.
  Memory is the 256 KiB held while an offer waits, at most 256 KiB of
  replies, `zmodem2`'s fixed buffers, a 64 KiB write buffer, and 100
  names per list. Progress events go at most every 256 KiB and 100 ms.
- File writes are buffered and done on the pump's task: a 64 KiB flush
  takes 48 µs at p50 and 0.8 ms at p99 on local disk. A slow network or
  removable disk would stall that session's pump; a writer thread per
  transfer is the fix if that shows.
- Protocol replies go to the session's input through a task of their
  own, so the pump never waits on the host.
- The view gets transfer events on one global channel; the backend sends
  file bytes nowhere but the folder.
- Limits of this slice: receive only; no resume (ZCRC); one folder,
  chosen by the app; no total size or file count per accept (Stop ends
  it, and a full disk fails the write); no macOS quarantine attribute;
  on file systems without hard links a local process can race the final
  rename; a session opened before the app's transfer host is set never
  transfers.
