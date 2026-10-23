# ADR 0020: Highlight sets and triggers

- Status: accepted
- Date: 2026-10-23

## Context

v0.4 colours what output matches (highlight sets, per host and group) and
runs actions on a match (triggers: notify, send text, capture). A pattern
is written by the user; the text it is matched against comes from the
remote host, which may be hostile or compromised. A backtracking regex
engine can take seconds on 30 characters chosen for it (`^(a+)+$` takes
3.4 s in V8), and the webview's main thread draws every pane.

A panel of three (security, performance, architecture) compared matching
the raw byte stream in the backend, JavaScript regexes over the
rendered rows, Rust regexes compiled to WebAssembly, and mixes of these.

## Decisions

### Rules and the engine live in the domain

A set holds up to 100 rules; a rule is a name, a pattern, an action, and
whether it is on. Patterns are the `regex` crate's syntax, which has no
backreferences or lookaround and matches in time linear in the text. Each
pattern compiles within 1 MiB, and a set's rules together within 32 MiB;
a set that cannot is refused when it is saved. The same code checks a
pattern as it is typed, when it is saved, and when it matches, so no rule
is valid in one place and wrong in another. `tauminus-domain` now depends
on `regex` and `regex-syntax`: pure matching and parsing, with no I/O.

Reading `triggers.json` checks counts first and each pattern's syntax, and
compiles nothing: a 6-byte `\w{20}` compiles to 1.2 MiB, and compiling a
file of 200 such sets on start took minutes and gigabytes. A pane compiles
only the rules it applies, at most 256. Sets that pass alone may pass the
limit together on one session: that pane shows no highlights, the error
is logged, and the failure is remembered until the library changes, so
output does not make it compile again.

Hosts and groups name sets in their settings, up to 8 per level, each
once. A session's sets add up from its host through each group upward,
nearest first, then the sets applied to every session in the order they
were added; at most 16, each once. A match that overlaps text an earlier
rule coloured is left out whole; whole-line rules are a layer of their
own. A session with no saved host (local, ad-hoc SSH,
serial), or whose host was deleted while it was open, gets the sets
applied to every session. Sets are kept in
`triggers.json`, apart from hosts, so sync (v0.5) treats them as it does
snippets.

### Matched against what the pane shows

The view sends the logical lines on screen (wrapped rows joined, empty
cells as spaces) and draws decorations where the answer says. Matching
the byte stream would not see what the user sees: Mosh and tmux redraw
with cursor moves, programs colour inside words, and progress bars write
over themselves with `\r`. The stream is not earlier either: the pump
waits on the view's acknowledgements.

A pane prepares once (`trigger_prepare`) and gets a handle to its compiled
rules; panes with the same sets share one, and at most 64 are kept. A
scan (`trigger_scan`) names the handle and reads no host. A change to the
library makes every handle stale, and the pane prepares again.

### Only the screen is decorated

Each xterm.js marker costs every line that scrolls past a call
(`Buffer.addMarker` registers a trim listener per marker): 51 live markers
took a flood from 22.3 to 18.8 MiB/s, 257 to 10.6. And each decoration
added or removed redraws the screen. So only what shows is matched and
decorated, at most 128 decorations; a scan keeps those that stay the same
and changes only the rest. Scans run at most every 100 ms while output or
scrolling goes on, every 400 ms while more than a screen scrolls between
two, one at a time, and not for a pane that is not on the page. A scan reads
at most 16 KiB of a line, 8 code points of a cell, and 256 KiB in all,
and answers with at most 512 hits: a host can stack thousands of
combining marks on one cell. The rules of a session share a 16 MiB DFA
cache; at 2 MiB, 100 rules thrashed it, from 0.1 ms a scan to 50 ms.

`\b` keeps its Unicode meaning, so it finds the edges of words like
"lỗi". The price: on a line that is not all ASCII, the engine cannot use
its DFA for it, and a screen with 20 such rules takes 4 ms to match
instead of 0.2 ms, 100 rules 20 to 47 ms, on a background thread. The
byte budget and the 256 rules a pane compiles bound it; `(?-u:\b)` asks
for the ASCII meaning and the fast path.

### Actions run in the view

The prod guard and the secret-prompt check run in the view, on what the
user types. Text a trigger sends will go through the same path as typing
(`TerminalController.type`), so the guard asks on a guarded host, the
text stays out of broadcast, and nothing is sent at a password prompt.
Highlight ships first; notify, capture, and send text follow with rate
limits per rule, no capture groups in sent text, and no actions on the
alternate screen.

## Consequences

- A highlight shows one IPC round trip after the text it colours.
- A match that spans more than the screen (a line wrapped past 64 rows
  above or below it) is not found, and such a line is matched from where
  its text was read, so `^` may match mid-line.
- Highlights stay on screen only: scrolled back to, a line is scanned
  again.
