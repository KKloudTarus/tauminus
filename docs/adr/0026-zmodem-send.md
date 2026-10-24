# ADR 0026: Files sent to rz

- Status: accepted
- Date: 2026-10-24

## Context

ADR 0025 receives files a host sends with `sz`. The other half is `rz`:
the host waits for files, and the terminal sends them. A panel of two
(security, architecture) weighed how the user chooses the files, how a
send fits the session pump, and whether trzsz belongs in v0.4.

A send carries a risk receive does not: our bytes go into the host's
input. If `rz` is gone when they arrive, the shell reads them, and a text
file's lines would run as commands.

## Decisions

### One gate for both directions

`find_offer` reads the header type after `**\x18B0`: `0` is ZRQINIT (`sz`
sends), `1` is ZRINIT (`rz` waits). One SIMD pass finds both. The offer
carries `Wants`, and the pump's existing gate applies unchanged: output
held, a numbered offer, 400 ms before the buttons act, keys refused, a
decline shows the held output, Ctrl+C declines or stops. An answer of the
wrong kind for the waiting offer does nothing.

### The user chooses in the system's dialog, opened by the backend

`zmodem_choose(session, offer)` opens `tauri-plugin-dialog` from Rust.
The webview is granted none of the plugin's commands, and no path crosses
IPC: the view gets names and sizes in events, and errors name a file by
its name only. One dialog is open at a time in the whole app, and only
for a `rz` offer that waits with that number, checked before the dialog
opens and again after it closes. The plugin replaces `alert` and
`confirm` in the webview with async ones the view may not call; lint
forbids them. ZRINIT alone
shows a banner; the dialog opens on a click, so `cat` of a file holding a
header pops nothing.

The SFTP pane already takes local paths from the view
(`transfer_upload`), so a compromised view could already copy a file out
over SFTP. This keeps Zmodem off that path; moving SFTP to the same model
is left for later.

### Files are opened once and checked on the handle

`DiskOutgoing::open` refuses a link, a folder, a device and a pipe from
the type of the open handle. On Unix it opens with `O_NOFOLLOW`,
`O_NONBLOCK` and `O_NOCTTY`, so a link or a pipe swapped in after the
look neither is followed nor blocks, and the handle must be the inode
the path named; on Windows a reparse point is refused. Only
the last path component goes to the host, at most 255 bytes, and a file
past 4 GiB (Zmodem's 32-bit offsets) is refused before anything is sent.
At most 100 files go per choice. Reads go through the handle, 64 KiB at
a time.

### A new sender after the choice

The ZRINIT held while the user chose is never replayed. A new
`Transmit` starts with ZRQINIT, and offers a file only to an `rz` that
answers it. If `rz` gave up meanwhile, the shell sees one line of `**`:
its echo of that header (raw, or `**^XB00` as a tty with ECHOCTL shows
it) ends the send at once, and with no echo the
first timeout (5 s) does: "rz on the host is not waiting any more".

### No line end leaves raw

`zmodem2` leaves CR, LF and most control characters raw in data, as
Zmodem allows. `Transmit` escapes every one of them as a ZDLE sequence,
as `sz -e` does; every receiver decodes those, with nothing to agree
first. Hex headers keep their line ends, as the protocol needs. What a
shell reads once `rz` is gone, however much it is, holds no line end, so
no line of the file runs. Every stop ends with Ctrl+E then Ctrl+U, which
drop that line: Ctrl+U alone kills only what is before the cursor, and
the cancel's CAN pairs are `C-x C-x` in bash, which puts the cursor at
the start. A live test kills `rz` mid-file under bash and presses Enter
with nothing before it: no line runs. The banner says Ctrl+C brings the
prompt back. Once files go, a damaged header from the host ends the send,
so `zmodem2`'s ZNAK, a hex header with a raw CR, never reaches a shell.
In bash, file byte 0x05 reaches readline as `C-x E`, which replays a
keyboard macro the user recorded, if any; the stream itself cannot
record one. On the wire, a log file grows 4.5% (its line ends) and random
data 28% (`overhead`, an ignored test, measures it); that is the price of
no line running.

### Backpressure, and a stop that does not reach the shell

The pump reads the file only while fewer than 128 KiB are queued for the
host, and starts again below 32 KiB; the writer task wakes it
("drained"), so a send to a silent `rz` keeps going. Each queued write
carries an epoch: any stop (the user's, a timeout, a read error, or the
host's own cancel, seen at once as five CAN even while the sender waits
for file data) starts a new one, and the writer drops older writes. The
SSH transport holds 64 writes the epoch cannot reach, so writes are at
most about 1 KiB: what a stop cannot recall stays near 64 KiB.
`zmodem2`'s streaming window is 1024 subpackets, 1 MiB per round trip:
128 held a 100 ms link to 0.9 MB/s, 1024 gives 4.9 MB/s. A host may ask
for a file's bytes again (ZRPOS back) 16 times; past that the send
stops. A send ends after 3 timeouts with no progress (data asked for, a
file taken), counted from the progress and not from host output, and at
once when the host prints 64 KiB without the send moving: a shell
echoing what reaches it, after `rz` died without a cancel.

The live tests send a file of `touch /tmp/pwned` lines, stop mid-way
(Ctrl+C, and `rz` killed by `timeout` mid-file), and check the file does
not exist on the host. A pump test has a
streaming `rz` stop by itself on a slow link: about 64 KiB reach it
after, against 121 KiB without the epoch cut.

### trzsz is not in v0.4

No Rust crate implements it. The npm library would run in the view, so
file bytes would pass through the webview, the recent ring and the
recording, which ADR 0025 rules out. A Rust domain implementation (a
line protocol with base64, MD5, zlib, and a directory mode) gets its own
ADR later.

## Consequences

- No transfer: one SIMD scan per chunk, as before; the type byte check
  adds nothing measurable.
- Sending: O(n) per byte sent; memory per send is 64 KiB read-ahead, at
  most 128 KiB queued plus about 64 KiB in the transport, 4 KiB of host
  input kept, and `zmodem2`'s fixed buffers.
- File reads happen on the pump's task, as receive's writes do.
- A new dependency: `tauri-plugin-dialog` 2.8.3 (Apache-2.0 OR MIT),
  with `rfd` and `tauri-plugin-fs`; `deny.toml` names them as the only
  wrappers of `tauri` and `rfd` they may be.
- Limits: no folders; no resume; the dialog cannot be tested in CI, so it
  is covered by the e2e fake and by hand.
