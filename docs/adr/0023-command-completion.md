# ADR 0023: Completing the command being typed

- Status: accepted
- Date: 2026-10-23

## Context

v0.4 completes commands from forked Fig specs on bash and zsh hosts. The
withfig/autocomplete specs (MIT, 715 commands) are TypeScript modules.
Many hold generators that run commands on the host, custom JavaScript,
and file templates. Fig ended in 2024 and the repository has not moved
since May 2025. A panel of two (architecture, and an advocate with
measurements) weighed the spec format, where matching runs, where the
typed line comes from, and the keys.

## Decisions

### Static specs, converted by hand, committed

`scripts/fig-to-json.mjs` converts 50 specs, the commands server work
reaches for, from a pinned commit (`specs/UPSTREAM`) into JSON. The JSON
keeps names, descriptions, subcommands, options and fixed values, and
nothing that runs: generators, custom functions, scripts and template
values go. The output is reviewed and committed, so no build runs
upstream code, and nothing is ever run on a host to complete a word. The
specs keep their MIT license (`REUSE.toml`, `LICENSES/MIT.txt`): 900 KB,
157 KB gzip, one lazily loaded chunk per command.

### Matching in the pane

The pane splits the line before the cursor into words, starting over
after `;`, `&&`, `|` and `$(`, and walks the command's spec. Commands
that run another (`sudo`) start over at it. It offers subcommands,
options not yet used, and an option's fixed values. A word that needs
quoting gets nothing, and so does a name that would need quoting. The
walk is O(w·f) for w words and a fan-out f below a few hundred: about
4 µs.

### Only where the shell says where the line starts

The line comes from the screen, from where the shell's OSC 133 B mark
put the cursor, to the cursor. Reading keys would miss what history,
Ctrl+R and the shell's own completion put on the line. Completion is
offered only while the shell waits at a marked prompt, on the normal
screen, not at a password prompt, and with nothing after the cursor.
A shell that marks no prompt gets none: guessing where a prompt ends
would complete the wrong word. So "bash and zsh hosts" means hosts whose
shell has OSC 133 integration. The marks are kept by the pane
(`ShellMarks`), with or without the history, which also fixes the AI
assistant's prompt check (ADR 0022) when the history is off. A B mark
counts only after a prompt's A, and Enter ends the prompt for a shell
that sends no C mark. Marks are a hint the host writes: a program can
print them, so they gate convenience, never a secret.

### The shell keeps its keys

The list opens only on a shortcut, Ctrl+Shift+Space. It takes no Tab,
arrow or Right key from a closed list. Ctrl+Space sends NUL, which Emacs
keys bind, and Right accepts zsh-autosuggestions' own suggestion. While
the list shows, Up and Down move in it, Enter or Tab take an entry, and
Esc closes it. Any other key closes it and reaches the shell as it would.
Taking an entry types the rest of the word and a space, without Enter,
as typed keys, broadcast like the rest of the word: the prod guard reads
the line at Enter as for any other. It types nothing if the line changed
since the list opened.

## Consequences

- No branch, pod, unit or file names: those need the host. The shell's
  own Tab still completes them.
- Hosts without OSC 133 get no completion until their shell is set up
  for it. Nor does a zsh with a right prompt or autosuggestions, which put
  text after the cursor.
- Combined short flags (`tar -xzf`) are not split, and an option's value
  after `=` is not offered.
- Upstream's `find` spec has BSD flags only.
- Some ops commands have no upstream spec (`journalctl`, `ip`, `ss`,
  `dnf`); adding one means writing its JSON.
- Updating the specs means running the converter on a newer pinned
  commit and reviewing the diff.
