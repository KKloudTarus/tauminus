# ADR 0012: Broadcast input and snippets on many hosts

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 adds running one snippet on many hosts, and broadcast input: keys
typed in one pane reach the other selected panes, with Ctrl+C reaching
them at once. A user broadcasts to a handful of panes; a tab holds at
most 16 and the window 64.

Each pane may be a prod host with its own guard, and the guard may be
holding input while it checks a line.

## Decisions

### What is broadcast

The shell keeps the set of broadcasting panes. What the user types in a
member goes to every other member through that pane's
`TerminalController.type`, so each pane's prod guard checks and asks for
itself. Replies the terminal makes for itself (cursor, device, mode,
and window reports, focus in and out, mouse reports, OSC and DCS
answers) stay in their pane: they describe that pane's screen, and a
host that could make its terminal reply into other panes could type
into them. Keys go only to members in the same tab, to panes whose
session runs, and, on a saved host, after its startup snippets: keys
sent while a pane connects would wait and arrive before those. A closed
pane leaves the set.

Keys a pointer makes stay in their pane too: wheel scrolling in a
full-screen program and Alt+click send arrow keys worked out from that
pane's screen. A paste is unwrapped and bracketed again in each member
as that member's shell asked, so a shell without bracketed paste never
sees the markers as keys. Ctrl+F3 and Shift+F3 encode the same as a
cursor report and stay in their pane; telling them apart would mean
tracking each pane's queries.

A secret is not broadcast. While the source pane's line asks for a
password or another secret, keys go only to members whose own line asks
for one too; the others are told they were held back. The decision holds
from the first key of the secret to Enter, Ctrl+C, or Ctrl+D, since a
prompt that echoes `*` (sudo's pwfeedback, on by default in Ubuntu
26.04) stops reading as a prompt after one key. For half a second after
a line is sent, the source's keys wait before they are broadcast, then
are judged by the screen as it is: a password typed ahead of its prompt
would otherwise go out before the prompt is drawn. A member's own screen
decides whether it asks for a secret, so a hostile host can print a
password prompt to receive one; it already receives every other key. Otherwise a
password for one host, at a sudo prompt that another host skips, would
echo there, run as a command, and land in its history. The palette adds or removes the focused pane, takes every pane in
the tab, or stops.

### Ctrl+C is never queued

Ctrl+C goes through `interrupt`, in every pane, broadcast or not. On a
guarded pane the guard sends it at once and drops what waited behind a
check, together with that check: the shell clears the line, so input
typed before Ctrl+C must not arrive after it, and a question about a
cleared line must not be asked. A question already on screen stays for
the user to answer.

### A snippet on many hosts

"On hosts" in the Snippets dialog picks saved hosts, up to a tab's 16.
The placeholders are filled once. Each host opens in its own pane of a
new tab, in a grid of about √n columns, and the panes broadcast
together, so the user can follow up in all of them. The splits are balanced, so every pane gets an equal share. The
snippet is typed into each pane once its session is up and its startup
snippets are typed, so they run first; a pane whose session ends before
it ever comes up never runs it, even after a retry, and the user is
told. Hosts that cannot connect or find no room are named in one
notice.

## Consequences

- A guarded pane in a broadcast asks for itself; the user answers each.
  Panes without a guard have already run the line when another pane
  asks.
- A question answered after a Ctrl+C sends nothing: the line it asked
  about is gone.
- Each member writes through its own input queue, so a key costs one
  IPC call per member: 320 calls a second at 16 panes and 20 keys a
  second. A batched write for unguarded members is the next step if
  that shows. The guard scans a paste with one regex pass, so a 1 MiB
  paste into 15 guarded panes costs about 90 ms, not 2 s.
- Broadcast and multi-host runs reuse the input path of a single pane,
  so the prod guard and the history see the same keys either way.
