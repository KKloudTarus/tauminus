# ADR 0017: Reconnecting, and tmux

- Status: accepted
- Date: 2026-10-23

## Context

A laptop sleeps, or its Wi-Fi changes, and every SSH session in it is
dead. TCP does not say so: a link that drops packets looks idle until a
write times out, which takes minutes. A user wants the pane to notice,
connect again on its own, and find their work where it was.

## Decisions

### A lost link is told apart from an ended session

An SSH channel that closes after an exit status, an exit signal, or a
close from the server ended. One that closes for any other reason (the
transport failed, a keepalive or a probe went unanswered) was lost. The
session reports `Lost` for the second, and the webview sees `lost: true`
on the exit. Telnet reports a read error as lost. A local shell and a
serial line never reconnect: there is nothing to connect to again.

### Noticing: rare keepalives, and a probe on waking

Each keepalive wakes the radio, so a quiet link is checked every 15 s and
given up after about a minute. The moment that matters is a wake or a
network change, and the app hears of it: the `online` event, or focus
coming back after 15 s or more away (a quick look away is no sleep). Each
running pane then probes its link. The probe goes around the input queue,
which a dead link may have filled; the SSH bridge pings the last hop and
reports the link lost if nothing arrives within 5 s. Output counts as an
answer, since the answer may wait behind it: a busy pane that has not
read its output for a while is not lost for it.

### The pane reconnects, the backend does not

The controller connects again after 1, 2, 5, 10, 15, then every 30 s,
each give or take a quarter so panes do not knock in step, for 25 tries
(about ten minutes). A session that drops within a minute of connecting
keeps counting, so a flapping host is given up on; so is one lost more
than ten times in half an hour, however long each session lasted. A wake
tries at once without using up a try.

At most six panes of one host connect at a time, under sshd's default
`MaxStartups` of 10; the others wait in line and each takes the slot the
moment one frees. A try that cannot reach the host (DNS, refused, a
timeout, the link dropping before or during sign-in) waits for the next.
A server that closes the connection itself during sign-in, as OpenSSH
does past `MaxAuthTries`, refused: another try would spend more of the
account's tries. One that is refused (authentication, a host key, the
vault, a server goodbye) stops, and
for a minute stops the host's other panes too without asking again.
Declining a question stops only the pane it was asked in. The user can
reconnect now or stop; stopping withdraws a question the try raised.

A stored password a server turns down is not sent again, by any pane or
to any host, for that user name until the app quits; the user is asked
instead. Sign-ins that would spend the same stored password take turns,
each waiting for the one before to hear the server's verdict, so a wake
after a password change on a directory server costs the account one
failed try, not one per pane. A goodbye right after the password counts
as turning it down; a link that only went away, or a later factor (a
one-time code) failing, does not. The password typed at the prompt and
taken, if it is the stored one, clears the mark. Only a keyed hash is
kept, under a key made at start and wiped on exit.

Host key checks, the vault, and prompts run as on any connect, so a
reconnect is never less checked than the first. The terminal keeps what
it showed and marks the gap with `[connection lost]` and
`[connected again]`. What the old session left open is closed first: a
control string, a link, the alternate screen, mouse reports, application
cursor keys, paste brackets. A prod guard question about to run in the
lost session is withdrawn, and keys typed while it is gone are dropped,
since they were meant for it. A broadcast secret latched to the pane is
forgotten.

### tmux keeps the work

A shell does not survive its connection. A host or group can name a tmux
session; each connect then runs
`exec "$SHELL" -lc 'exec tmux -u new-session -A -s NAME'` in place of the
shell, which makes the session the first time and joins it after. The
login shell sets the `PATH` that finds tmux (Homebrew's, for one); `-u`
is passed when the host's character set is UTF-8. The name is limited to
`[A-Za-z0-9_.-]{1,64}`: no quoting is needed, and `#` (a tmux format,
which can run a command) cannot appear. A host or nested group can turn a
parent group's session off. Startup snippets do not run on a host that
joins tmux, since the session may already run something, from another
pane or an earlier day. Nor do they after any automatic reconnect: no one
is watching what they would be typed into.

`hosts.json` becomes version 6 only when a tmux setting is saved.

## Consequences

- A session without tmux comes back as a new shell; what ran in the old
  one ended with it.
- Telnet has no probe: a dead Telnet link is found by the next write, or
  by TCP after many minutes. A Telnet server that ends a logout with a
  reset instead of a close reads as lost, and the pane offers to connect
  again.
- Several panes on one tmux session, broadcast to together, type each key
  into it once per pane.
- With tcsh as the login shell, `-lc` may not load the login `PATH`, and
  tmux may not start.
- Mosh, which keeps a session over a changing network itself, is a
  separate transport.
