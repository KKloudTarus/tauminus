# ADR 0011: Snippets

- Status: accepted
- Date: 2026-10-23

## Context

v0.3 adds snippets: saved commands in folders with `{{variable}}`
placeholders, run in a terminal, and startup snippets a host types into
each new shell. A user keeps tens to a few hundred; the library is
capped at 2000 snippets and 8 MiB of bodies, each body at 64 KiB and 32
placeholders. The byte cap keeps the file, with JSON's escapes, under the
16 MiB the store reads.

A snippet is text that becomes keys in a shell. On a prod host that is
the same risk as typing it, so it must meet the same check.

## Decisions

### Placeholders

`{{name}}` asks for a value and `{{name:default}}` offers one; a blank
value counts as none, so `rm -rf /srv/{{site}}` never runs without its
site. Names start with a letter or `_` and go on with letters, digits,
`_`, `-`, and `.`, up to 64, so a Go template's `{{.Names}}` stays text.
A word such as `{{end}}` is a placeholder; `{{{{end}}` types it as is.
A default is one line of up to 1 KiB. `{{{{` writes `{{`. Anything else that only looks like a
placeholder stays text, so a shell's `${x}` or a Go template is sent as
typed, and later placeholders and escapes still work. A filled value is
not parsed again. Parsing is one pass, O(n): the next `}}` is remembered
and looked for again only once the scan passes it.

A body holds no control characters but tab and line breaks, and a value
or default none but tab. Ctrl+A, Ctrl+Y, or an ESC sequence would act as
editing keys, so the line the shell runs would differ from the text the
prod guard checked. A value is at most 4 KiB, names a placeholder the
body uses, and a filled snippet types at most 1 MiB: a body can use one
placeholder thousands of times.

Rendering lives in the domain crate. The view asks the backend for the
text to type, so there is one implementation of the rules.

### Folders are one level

A snippet has an optional folder name. One level covers the libraries
users keep; nested folders would add moves, renames of subtrees, and a
tree editor for no workload that needs them.

### Storage

The library is its own file, `snippets.json`, written whole and
atomically as `hosts.json` is, with the same check that refuses to
overwrite another window's change. Snippets are a different kind of data
from hosts and change on their own.

A host lists its startup snippets by id, at most 8, in `hosts.json`. The
file becomes version 3 only when some host has them, so an older build
keeps reading a file that does not use them. A startup snippet deleted
since is skipped, as is one with a placeholder that has no default; the
user is told which and why.

### Running goes through the prod guard

A run types the snippet into the focused pane as keys, each line ended by
Enter, through `TerminalController.type`. On a guarded host that is the
prod guard's input path: text with Enter in it is a paste, and a paste
always asks, with what the check found. Text already on the command line
runs with it, so the guard checks and shows a paste that ends a line
together with that text. Startup snippets take the same
path each time the session comes up, reconnects included.

## Consequences

- One parser serves the view, the backend, and startup.
- A prod host asks before each snippet, including its startup ones. That
  is the guard's rule for pasted lines, and a snippet is one.
- A snippet is typed as keys, so a tab in it asks the shell to complete,
  as it would if typed.
- Startup snippets are typed when the session opens, not when the first
  prompt shows. A host that asks for a new password or shows a menu
  first would read them as answers; waiting for the prompt mark (OSC 133)
  is the next step if that proves common.
- Running a snippet on many hosts at once, and broadcast input, build on
  `type` and are a separate change.
