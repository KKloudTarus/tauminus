# ADR 0006: The prod guard reads the screen and over-approximates the shell

- Status: accepted
- Date: 2026-10-22

## Context

A user typing into a production host can run `rm -rf`, `DROP TABLE`, or
`kubectl delete` by mistake, or recall one from history with the wrong
host in front. Termius has no guard. The guard has to see the line the
shell is about to run, decide whether it is dangerous, and ask before Enter
reaches the server. It runs on every Enter, so it must answer in
milliseconds, and a miss costs far more than a false alarm.

## Decisions

### Read the line from the screen, not from keystrokes

Rebuilding the line from keystrokes breaks on history recall, tab
completion, `Ctrl+R`, and every editing key the shell binds. The screen
shows what the shell's line editor holds. When the user starts typing, the
terminal marks the cursor position with an xterm.js marker, which follows
scrolling and reflow. On Enter, the line runs from that mark to the end of
the cursor's wrapped line. The whole line, prompt included, is checked as
well, so a lost or shifted mark can only add findings, never hide one.

The screen lags the shell on a slow link. Enter therefore waits up to
0.5 s for the echo of the keys before it, and the printable keys typed since
the prompt are checked too. The rows with text right below the line are
checked as well, for a recalled multi-line command. Each source can only add
findings.

Input that arrives while a check or a question is pending waits behind it,
so keystrokes stay in order. A line the shell will continue (an open quote,
a trailing `|`, a here-document) is kept and checked again together with the
next line. A paste of more than one line always asks. A paste holding other
controls, such as Ctrl+C, asks too, and each piece between them is checked
as its own line, since the line editor acts on them as keys. Every terminal
removes paste markers from inside a bracketed paste, so pasted text cannot
end the paste early.

### A hand-written lexer in the domain crate, not `brush-parser`

A panel compared `brush-parser` 0.4.0 with a hand-written lexer, both with
evidence from their code:

- `brush-parser` brings 63 crates, including `insta` and `tempfile` as normal
  dependencies, into a crate that so far depends on `thiserror` alone.
- Its tokenizer returns an error and no tokens for incomplete input, so
  `rm -rf / ; echo "x` would hide its `rm`. Incomplete input is the normal
  case for a guard that runs on each Enter.
- It still leaves `$'\x72m'` undecoded and `bash -c` unread, so the guard
  needs its own word handling either way.

The lexer follows POSIX token recognition (section 2.3) and bash quoting.
It decodes ANSI-C quoting, expands braces, joins line continuations, reads
here-document bodies, and hands back the text of every substitution to be
read as a script of its own. It never fails: what it cannot resolve, such as
`$X` or `/bin/r?`, it marks on the word. One pass is O(n). Brace expansion
pairs braces with a stack, O(L) per step and at most 256 steps per word.

A review found inputs that cost seconds: nested scripts that each start
more, and brace bombs. Every check now has a budget of 8 × 64 KiB lexed
characters across all nesting, brace expansion included, and of 64 Mi
character comparisons for pattern matching; past either the line is
flagged unread. Patterns without `?` match without backtracking. The
slowest adversarial 64 KiB line takes under 0.1 s in a release build. The check
runs on a blocking thread, and the view asks the user after 3 s.

### Over-approximate what may run

Every word of a simple command is also read as a possible command name, so
wrappers the guard does not know, such as `docker exec web rm -rf /`, still
show their command. Names that are dangerous without arguments (`reboot`,
`mkfs`) count there only after a word that runs what follows, or
`grep reboot syslog` would ask. Each such position reads at most 64
arguments, which keeps a line O(w) in its words.

Scripts inside scripts are read too: `bash -c`, `sh` fed by a
here-document or here-string, `eval`, `su -c`, `ssh host '...'`, `watch`,
`env -S`, and the code of `python -c`, `perl -e`, `node -e`, and `awk`. Text
piped into a shell is flagged, since `echo ... | base64 -d | sh` hides its
script. A command name that only the shell can resolve is flagged rather
than guessed.

### Rules live in the backend; the view only asks

The webview sends the line and the host id; the backend reads the host's
policy from the store as it is at that moment, so a rule saved a moment ago
applies to the next Enter. The line is never logged. A failed check asks the
user, as a finding would.

### Patterns are globs over the command as it runs

Patterns match the command after quotes are removed and wrappers such as
`sudo` and `env` are skipped, so `sudo "rm" -rf x` matches `rm -rf x`.
Patterns from the host and every group above it apply, at most 100 of each
kind per level. Deny patterns match the whole command, so they match more.
Allow patterns match word by word after paths are resolved, and only
literal words match, so `rm -rf /tmp/*` allows neither `rm -rf /tmp/x /`,
`rm -rf /tmp/../etc`, nor `rm -rf /tmp/$X`. They must start with a command
name, so `*` cannot turn every rule off. An allow
pattern silences built-in rules for the command it matches, never a deny
pattern and never the other commands on the line.

## Consequences

- The guard cannot see aliases or functions defined on the server, scripts
  run by file name, or programs that delete data without a telling command
  line.
- On the 194-line safe corpus, one line asks: `echo "DROP TABLE is
  dangerous"`. `rm` with a word that starts with a variable, such as
  `rm -f "$f"`, asks too: rm reads any word starting with `-` as options.
  `rm -f -- "$f"` does not ask.
- A host whose `prod` tag is added while a terminal is open is guarded from
  the next Enter; the frame and status bar follow the host list.
