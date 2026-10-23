# ADR 0014: Character sets per host

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 lets a host's shell speak a character set other than UTF-8, as
older Unix systems and network gear do. xterm.js reads UTF-8 only, and
the prod guard, the history, and OSC handling all read what xterm.js
decoded.

## Decisions

The setting is a closed list: UTF-8, Windows-1252 (which ISO-8859-1
hosts read), ISO-8859-2, Windows-1251, KOI8-R, Shift_JIS, EUC-JP,
GB18030, Big5, and EUC-KR. Each keeps ASCII as it is, so escape
sequences and control keys pass through untouched. UTF-16 and
ISO-2022-JP do not, and are left out. A host inherits it from its
groups; the root default is UTF-8. `hosts.json` becomes version 4 only
when some host or group sets one.

The SSH bridge converts, one converter per session, with `encoding_rs`
(Apache-2.0 or MIT, plus BSD-3-Clause for its data; no dependencies of
its own past `cfg-if`). Output's decoder holds a character split across
chunks until its last byte arrives, and shows a byte sequence the set
has no character for as U+FFFD. Converting there means the session pump,
flow control, and acknowledgements count the UTF-8 bytes xterm.js
parses. Local shells speak the system's UTF-8 and are not converted.

Typed text and raw bytes now travel apart: `TerminalInput::Text` is
converted, `TerminalInput::Data` (legacy mouse reports) is not. A
character the set cannot hold is sent as `_`. `encoding_rs`'s default
replacement is an HTML entity, which a shell would take as typed text,
and `?`, which other terminals send, is a shell pattern: `rm ~/tmp/✓`
would become `rm ~/tmp/?` and remove every one-letter name there.

No C1 control crosses in either direction. ISO-8859-2 maps 0x80 to 0x9F
to U+0080..U+009F, and the Windows sets map a few of those bytes there
too; xterm.js acts on them (U+009B is CSI, U+009D is OSC), so a stray
byte in a file shown on such a host would drive the terminal. Decoded
output shows them as U+FFFD, and typed ones are sent as `_`.

## Consequences

- The prod guard and the history see what was typed; a host whose set
  cannot hold a character receives `_` in its place.
- A UTF-8 host costs nothing: bytes pass through as they are.
