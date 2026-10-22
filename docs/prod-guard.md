# Prod guard

Hosts tagged `prod` get a red frame, a `PROD` badge on their tab, and a red
status bar. Before Enter sends a line to such a host, Tauminus checks it and
asks when it looks dangerous. A paste of more than one line always asks.
The dialog shows the host and the exact text, with control and
bidirectional characters made visible. Cancel has focus, so a reflexive
Enter cancels.

Why it works this way is in [ADR 0006](adr/0006-prod-guard.md).

## Turning it on and off

The guard is on for a host tagged `prod`. The **Prod guard** section of a
host or group editor overrides that: **On** guards a host without the tag,
**Off** turns it off. The nearest level that sets it wins, as for other
settings.

## What asks

| Rule | Catches |
|---|---|
| `recursive-delete` | `rm -r` (also `--recur` and other abbreviations), `rm --no-preserve-root`, `find -delete`, `rm` with a word that starts with a variable, such as `rm $FLAGS` or `rm "$opt"` (write `rm -- "$f"` to say no options follow), and tree deletes such as `shutil.rmtree` in `python -c` code |
| `drop-data` | SQL `DROP TABLE/DATABASE/SCHEMA/...`, MongoDB `dropDatabase()` and `.drop()` |
| `empty-data` | SQL `TRUNCATE`, Redis `FLUSHALL` and `FLUSHDB` |
| `delete-all` | `DELETE FROM` without `WHERE`, MongoDB `deleteMany({})` |
| `kubernetes-delete` | `kubectl delete` (also `k`, `oc`, `kubecolor`) unless `--dry-run`, `helm uninstall` |
| `infra-destroy` | `terraform destroy`, `terraform apply -destroy`, `tofu`, `terragrunt`, `pulumi destroy` |
| `power-off` | `shutdown`, `reboot`, `halt`, `poweroff`, `systemctl reboot`, `init 0`, writes to `/proc/sysrq-trigger` |
| `format-disk` | `mkfs*`, `mke2fs`, `mkswap`, `wipefs` |
| `disk-write` | `dd of=/dev/...`, `> /dev/sd*` and other block devices, `tee`, `cp`, or `shred` onto one |
| `open-permissions` | `chmod`, `chown`, or `chgrp` with `-R` on `/` or a top-level directory |
| `pipe-to-shell` | `curl ... \| sh`, `bash <(curl ...)`, `source <(...)`, `xargs sh -c`, `... \| at now` |
| `opaque` | a command name only the shell can resolve: `$CMD`, `$(echo rm)`, `/bin/r?`, `!!`, `^old^new` |
| `unreadable` | lines over 64 KiB, nesting deeper than 8 levels, more than 4096 commands, more work than 8 × 64 KiB of text, or a host whose groups give it more than 400 patterns |
| `denied` | a deny pattern of the host or one of its groups |
| `edit-and-run` | Ctrl+X Ctrl+E, or Escape then `v` in vi mode, which open the line in an editor and run what it saves |
| `comment-and-run` | Alt+#, which toggles a comment on the line and runs it |
| `control-characters` | a paste holding Ctrl+C, Escape, or other keys that act in the line editor |
| `unchecked` | a check that failed or took more than 3 s |

Rules apply after quotes, escapes, `$'...'`, and braces are resolved, through
`sudo`, `env`, `nice`, `xargs`, `timeout` and similar wrappers, and inside
`$(...)`, backquotes, `bash -c`, `eval`, `su -c`, `flock -c`, `script -c`,
`ssh host '...'`, `tmux new '...'`, `trap '...'`, `alias`, `complete -C`,
`PROMPT_COMMAND`, here-documents and here-strings (also fed to `ssh`), and the code of `python -c`, `perl -e`,
`node -e`, and `awk`. A quoted argument with spaces is read as a possible
script too, for runners the guard does not know, except for commands whose
arguments are text, such as `grep`, `git`, and `echo`. SQL rules read the whole
line, so they apply at a `psql` or `mysql` prompt.

## Which line is checked

The guard reads the line from the screen, from where you started typing to
the end of the wrapped line, and also reads the whole line, the rows with
text right below it, and the keys you typed. A finding in any of them asks.
On a slow link, Enter waits up to 0.5 s for the echo of the keys before it.
In a full-screen program such as vim or tmux, only the cursor's line is read.

At a prompt that asks for a password, passphrase, PIN, token, secret, OTP,
or verification code, Enter goes through unchecked when only printable keys
were typed and none of them showed, even after the echo wait: those keys are
a secret, not a command. A recalled or pasted line there is still checked.

A line the shell will continue, such as one with an open quote, is checked
again with the next line. Ctrl+C starts over.

## Allow and deny patterns

Patterns are one per line. `*` matches any run of characters and `?` one
character. They match the command as it would run, with quotes removed and
`sudo` and similar wrappers skipped:

- **Always ask before** (deny): `systemctl stop *` asks for any unit. Deny
  patterns match the whole command, so `*` may cross spaces.
- **Never ask before** (allow): `rm -rf /tmp/build/*` lets that cleanup run
  without a question. Allow patterns match word by word after paths are
  resolved, so the same pattern still asks for `rm -rf /tmp/build/x /` and
  `rm -rf /tmp/build/../../etc`. Only literal words match: a variable, a
  glob, a leading `~`, or a `..` part keeps asking. An allow pattern must
  start with a command name. It never silences a deny pattern or the other
  commands on the line.

Patterns from the host and every group above it add up, at most 100 of each
kind per host or group. Removing a group whose patterns would push a member
past 100 is refused. A host that gathers more than 400 in all asks before
every line, since checking them all could take too long.

## Limits

The guard cannot see aliases or functions defined on the server, scripts run
by file name, or programs that delete data without a telling command line.
The guard never logs a line it checks.

## Tests

`crates/tauminus-domain/tests/guard/` holds the corpus that CI runs:

- 322 dangerous lines, each with the rule it must trigger. None may be
  missed.
- 194 ordinary lines. One asks (`echo "DROP TABLE is dangerous"`), a false
  positive rate of 0.5%. The test fails above 3%.

Unit tests cover the lexer, every rule, patterns, incomplete input, random
input over shell metacharacters, and adversarial 64 KiB lines with 100 allow
and 100 deny patterns that backtrack as much as patterns may. The slowest of
those takes under 0.1 s in a release build.
