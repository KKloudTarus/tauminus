# ADR 0005: Saved hosts, their file, and importing ssh_config

- Status: accepted
- Date: 2026-10-21

## Context

Termius users expect a host list with groups, tags and shared settings.
Most of them already have an `~/.ssh/config`. The list must survive
crashes, a second window, and a damaged file, and nothing in it may let a
compromised view run commands.

## Decisions

### One JSON file, not a database

Hosts and groups live in one `hosts.json` in the app's config directory. A
list tops out at 10 000 hosts; a typical one of that size is 2.5 MB and
saves in 44 ms, at the rate a person edits. SQLite would add a native
dependency and migrations for data that is read whole and written whole.
The file is versioned, rejects unknown fields, and a version newer than the
build is refused rather than misread.

### Atomic writes and refusing to overwrite

A save writes a temporary file with an unguessable name, created exclusively
(so a planted symlink is not followed), flushes it, renames it over the old
file, and flushes the directory. A file that does not parse is reported and
never overwritten. Before writing, the store checks that the file still
holds what it last read; a change from another window makes the save fail
and the list reload, instead of the last writer winning.

### Nearest setting wins; the route is one setting

Settings inherit from the host up through its groups, nearest first, as a
specific `Host` block comes before `Host *`. Jump hosts and the route are
resolved together, from the nearest level that sets either: a host with its
own ProxyCommand must not pick up a group's jump hosts. Deleting a group
copies what its direct children inherited from it into them.

### `ssh -G` is the reference for imports

Each alias is resolved by running `ssh -G`, which applies the system config,
`Match` blocks, and every keyword, exactly as the user's `ssh` would. An
alias it rejects is reported with OpenSSH's message, never guessed. Without
an `ssh` binary, a built-in resolver ports `argv_split`, first-value-wins,
`IdentityFile` accumulation, conditional `Include` and the ProxyJump versus
ProxyCommand rule from `readconf.c`, and agrees with `ssh -G` on 33 configs
in the test corpus. It treats `Match` blocks other than `Match all` as not
matching.

`ssh -G` evaluates `Match exec` lines, as every `ssh` call does; the import
button says so. Each run gets its own process group and is killed with it
after 5 s; the whole import stops after 30 s.

### ProxyCommand stays out of the view's reach

A ProxyCommand is run by a shell. Only the importer, reading a file the
user wrote, may create one. `ssh_open` refuses one, saving a host may keep
an existing ProxyCommand unchanged but not add or alter it, and saved hosts
connect by id through `host_connect`, which reads the target from the store.

## Consequences

- A host list made on another machine with a newer Tauminus does not load
  until this one is updated.
- Editing a ProxyCommand means editing `~/.ssh/config` and importing again,
  until a native confirmation dialog exists.
- Two windows editing at once see one save refused and retried; neither
  loses the other's change.
