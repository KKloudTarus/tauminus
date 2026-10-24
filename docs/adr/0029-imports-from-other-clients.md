# ADR 0029: Hosts from other SSH clients

- Status: accepted
- Date: 2026-10-24

## Context

v0.5 imports hosts from a CSV list, PuTTY, MobaXterm and SecureCRT. The
`~/.ssh/config` import already turns entries into saved hosts through the
domain's checks. Only PuTTY and SecureCRT document their formats; the
field positions below come from their source (PuTTY, KiTTY), VanDyke's
notes and real files, and for MobaXterm from a format read from v23.6
files (`gplv2/mobaxterm2ssh`) and the importers that agree with it.

## Decisions

### One import, many readers

`crates/tauminus-import` holds a reader per format. Each turns bytes into
the `ConfigHost` entries the `~/.ssh/config` import makes, and
`HostBook::import` saves them through the same checks. A source names its
group (`PuTTY`, `MobaXterm`, `SecureCRT`, `CSV import`) and what an error
calls it. That group is a top-level one: a folder another import brought
under the same name is never taken for it. Folders the other client kept
become groups inside it, at most 15 levels below; importing again reuses
them and skips hosts already saved by name. A group is made only for a
host that is saved, so a skipped host leaves no empty one. When the group
list is full, a host goes into the deepest group there is room for, and
the import still saves. No crate is added: the readers are a few hundred
lines, and the text of a file is read by `tauminus-charset`, the one
place that converts character sets.

### What is read, and what is not

- **Text.** UTF-8 with or without a BOM, UTF-16LE with a BOM or without
  one (`regedit` writes it), else Windows-1252, as MobaXterm writes.
- **CSV.** A header names the columns; the names Termius, mRemoteNG and
  Devolutions write are taken in any case. The separator is the first of
  `,`, `;` and tab in the header. Quoting follows RFC 4180. mRemoteNG's
  containers (`Id`, `Parent`, `NodeType`) become folders.
- **PuTTY.** The registry on Windows, a `.reg` export (5.00 or REGEDIT4),
  or `~/.putty/sessions` on Unix, found in PuTTY's own order. `%XX` names
  are read in either case of hex. `user@host` in `HostName` is split.
  `ProxyMethod` 6 (an SSH jump) becomes a jump host.
- **MobaXterm.** `[Bookmarks*]` sections, `SubRep` as the folder, type 0
  (SSH) sessions: host, port, user, the jump host chain, the key.
- **SecureCRT.** The `Sessions` folder's `.ini` files, `S:` and `D:`
  lines, the SSH2 port, the key when the global one is off (named by its
  `.pub` half; the private one beside it is taken), and a `Firewall Name`
  of `Session:<path>` as a jump through that session, followed through
  sessions that jump in turn, at most eight. A loop, a missing session, or
  a hop that is not SSH2 is reported.
- **Host names** as PuTTY reads them (`prepare_session`): `user@` in
  front wins over the user field, a `:suffix` after a name with one colon
  is dropped, spaces go. The jump host likewise.

Each of these is reported, not saved, with its reason: a session that is
not SSH (or SSH1), a SOCKS, HTTP or Telnet proxy, a local proxy command,
a named SecureCRT firewall. A local proxy command in particular is never
imported: it would run a program on each connect, and only
`~/.ssh/config`, which the user's own `ssh` already runs, may bring one.
Passwords are never read, encrypted or not. A PuTTY key (`.ppk`) is left
out: the SSH adapter reads OpenSSH keys, and Pageant still signs for it.
A key on another machine (`\\server\share`, `//server`, a device path)
is reported: reading it at connect would make Windows sign in to that
machine with the user's credentials. So is a jump host with no name or
named `none`, which `ssh -J` would read as no jump, and a port that is
not one.

Every reader is bounded by what the app keeps: at most 15 folder levels
and 128 characters a name, 256 fields a CSV row, 20 000 sessions. A file
of 16 MiB (64 MiB for a UTF-16 `.reg` export) reads in about 0.1 s and
imports in under 1 s at the 10 000-host limit. The view gets the first 50
skipped hosts, each name and reason cut to 128 characters with bidi
controls removed, and the count of the rest; a label may not hold a bidi
control either.

### The user picks the file in the system's dialog

`hosts_import_from(kind)` opens the dialog from Rust, as Zmodem does
(ADR 0026), under the same one-dialog-at-a-time guard, so no path crosses
IPC and the view cannot name a file to read. A file is read only if it is
a regular file, checked before it is opened, so a pipe picked by mistake
does not hang the import. SecureCRT's folder is walked without
following links, at most 16 levels, 20 000 files and 64 MiB in all.
PuTTY's own sessions need no dialog.

## Consequences

- Moving from PuTTY, MobaXterm, SecureCRT, Termius (CSV) or mRemoteNG
  keeps the hosts, their folders and their jump hosts.
- A `~/.ssh/config` group the user moved inside another group is not
  found by a later import, which makes a new top-level one for new hosts.
- MobaXterm's format may change without notice. A line from a version
  that writes fewer fields still reads; one that moves fields would read
  wrong values, which the domain's checks catch only when they are
  invalid.
- SecureCRT's XML export and KiTTY's portable files are not read yet.
