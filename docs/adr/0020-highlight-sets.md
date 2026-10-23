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

### Triggers run in the view, on finished lines

A rule's action is a highlight, or one of three triggers: notify, keep the
line (capture), or send text. Triggers have their own matcher: each fires
once per line, at its first match, and rules do not hide each other.

Notify and capture read each line of output once, when the cursor has
left it: a progress bar redrawn with `\r` fires once it is done. A scan
reads at most 512 rows every 100 ms; a flood faster than that skips lines,
since triggers watch, they do not guard. An erased screen (Ctrl+L) counts
what it erased as read. Send reads the line the cursor waits on, once
output has stopped for 300 ms, and each such line once: a question like
`Continue? [y/N] ` waits there, and a password prompt that follows a line
arrives before anything is typed. Full-screen programs (the alternate
screen) are skipped. Lines come back with secrets of a known shape hidden,
as the command history hides them; a line that is all base64 (one line of
a private key, whose BEGIN line came alone before it) is hidden whole, and
characters that change how text reads unseen are dropped.

- **Notify.** The app's notice shows the rule, the pane, and the line in
  quotes, once a rule a scan and at most every 5 s, so the app's own
  notices are not drowned out. A system notification shows only while
  the window is in the back, at most one per rule every 30 s and five a
  minute in all, and holds no output: notification centres keep what they show, on the lock screen
  too, Linux servers render markup in bodies, and output could pose as a
  prompt of the app's. The webview names the rule (a handle and an
  index); the backend writes "Tauminus: rule" and "Matched on host" from
  what the user wrote, and raises it through `tauri-plugin-notification`
  from Rust. The window has none of the plugin's permissions, and a test
  refuses any permission in the capability file that is not an app
  command.
- **Capture.** The line goes to a list in memory, at most 1000, newest
  first, to read and copy. Nothing goes to disk.
- **Send.** The text holds no control characters (Enter is its own
  choice) and nothing from the match: the host would choose what runs. It
  is typed as the user types (`TerminalController.type`), so the prod
  guard asks on a guarded host, it enters the history, and it is not
  broadcast. It is not sent at a password prompt, into a full-screen
  program, or over a line the user has begun to type (with Enter, the
  user's half-typed command would run). A rule sends at most every 2 s
  and five times a minute in a pane; past that it is off in that pane
  until the rules change, which ends a loop on its own echo. On a
  guarded host the check is made when the rule fires, not again when the
  user confirms the guard's question.

## Consequences

- A highlight shows one IPC round trip after the text it colours.
- A match that spans more than the screen (a line wrapped past 64 rows
  above or below it) is not found, and such a line is matched from where
  its text was read, so `^` may match mid-line.
- Highlights stay on screen only: scrolled back to, a line is scanned
  again.
- A trigger fires only on output that arrives while the pane is open, not
  on what it showed before, and a line wrapped across more than 512 rows
  is read from its last 512.
