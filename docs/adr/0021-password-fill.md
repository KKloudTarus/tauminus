# ADR 0021: Typing a saved host's password at its prompt

- Status: accepted
- Date: 2026-10-23

## Context

v0.4 suggests a password from the vault when a pane shows a password
prompt: `sudo` or `su` on a saved host. The host writes the prompt, and a hostile one can print `Password:` at any time,
name another user in it, or echo what is typed. A panel of two (security,
architecture) weighed which identities to offer, how the password travels,
and what must hold first.

## Decisions

### Only the host's own login identity, chosen by the backend

The vault types the login identity of the saved host the session was
opened from, and no other. When a saved host's SSH or Mosh session opens,
the session records the host, the identity, and the user and address it
logged in with (`SessionMeta.host`). The view names the session, not an
identity. The backend types the recorded identity's password, and only
while the host is still saved with that identity and address: an edited
host does not send its new password to the server the old session is on.
Identities named by the prompt's text or chosen from a list are left out:
the host writes the prompt, so it would pick the secret it gets. Sessions
with no saved host (local, ad-hoc SSH, serial) get none. Telnet is
refused: it carries what is typed in the clear.

### The password never reaches the view

`session_fill_password` writes the password into the session as typed
text (`TerminalInput::Secret`, wiped when it drops) and returns nothing.
It goes through the pane's input queue as a chunk of its own, in turn with
keys typed before it, never merged with them. It is not broadcast, and the
prod guard and the history never see it.

### Only at a prompt that asked, with nothing since

The backend decides from the session's own output: it keeps the last
512 bytes as they arrive and types only if they end on a prompt that says
whose login it asks for (`tauminus_domain::password_prompt`), and that
login is the session's. Only three shapes qualify: sudo's
(`[sudo] password for ops:`, and its translations), on the host itself,
and OpenSSH's (`ops@db's password:`, `(ops@db) Password:`), naming this
user and this machine. User names compare exactly, host names without
case. Everything else gets nothing: a bare `Password:` (a second hop,
`su`, telnet, a database client, a router all print one, and it says
nothing of whose password it wants), a passphrase, a code, a new
password, a question. Each prompt is filled once, and not if output came
that the view had not parsed when it asked.

The pane offers a chip ("Type the password of deploy (ops@db)", or
Ctrl+Shift+L) when the cursor's line, on the normal screen, has stood for
60 ms as such a prompt with nothing typed after it, and the backend
agrees. The offer goes as soon as the line changes or anything is typed
on it, broadcast keys included. Asking for an offer does not count as use
of the vault, so output cannot keep it from locking when idle.

### Enter stays the user's

The password is typed without Enter. At a real prompt it shows nothing
(or a `*` a key), and one more key sends it. At a fake one, on a host that
echoes, it shows on the line but has not run and has not entered the
host's shell history: the pane says so, and Ctrl+U takes it back.

## Consequences

- The checks compare names that programs on the host print. A second
  hop that prints this user and this machine's name (`ssh db` from
  inside `db`, an alias of that name, a name split DNS resolves to
  another box) is not told apart, and gets the password at a click. So
  can any program on the host that prints one of the three shapes.
- The backend sees only bytes, so a webview that is not the app's own
  could have the session's own host's password typed at such a prompt.
  It cannot pick another identity or another session.
- `su` is not filled: its prompt names no one, and it wants the target
  user's password, not the login's.
- While the vault is locked, whether the identity holds a password is not
  known: the offer stands, and after the unlock the fill may say it has
  none.
- After a reconnect, output of the old session still waiting to be drawn
  counts toward what the pane has seen; the prompt check in the backend
  still applies.
- Mosh draws its screen with cursor moves; when the prompt does not end
  its output as a line, no offer is made there.
- A host whose login identity holds a password can always print a fake
  prompt and, with the user's click, get that one password. The click,
  the single identity, and the echo warning bound what it gets.
- Echoed bytes stay in what the pane recorded (session recording, trigger
  captures) like any other output; the warning tells the user to change
  the password.
