# ADR 0013: Saved workspaces and session status dots

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 adds saved workspaces, which open a set of tabs again with their
splits and hosts, and status dots, which show what happened in a tab
while it was hidden. A window holds at most 64 panes and a tab 16.

## Decisions

### Workspaces live in the window's local storage

A workspace is layout and host ids: no secrets, nothing another device
needs. It sits in the webview's local storage beside the theme and key
bindings, which are the same kind of state. A backend store would add a
file format, a port, and IPC for no reader but this window. At most 50
are kept.

What is read back is checked field by field, with depth and pane counts
bounded, and anything malformed is dropped. A layout is repaired as it
opens: a split of one child becomes the child, a child split in its
parent's direction is folded in, and sizes that do not fit are shared
equally, so every layout keeps the invariants of ADR 0003.

A saved host is kept by id and opens as it is saved now; one deleted
since opens a local shell, and the user is told. A typed-in connection
keeps its endpoint and jump hosts only. One through a proxy is saved as
a local shell: a proxy command read back from storage would run
whatever was written there.

### Status dots

Each pane notes, while its tab is hidden, the most urgent of: output
arrived (blue), the shell asks for input (amber: a new prompt from shell
integration, a bell, a connect prompt, or a prod guard question), or a
command or the session failed (red: OSC 133 with a non-zero status, or
a failed or non-zero exit). Showing the tab clears them. A tab shows
the most urgent of its panes. Noting is O(1) per output chunk and
notifies only when the level rises.

## Consequences

- Workspaces do not follow the user to another device; sync is v0.4's.
- Without shell integration, "waiting" comes only from a bell or a
  prompt the app itself shows.
