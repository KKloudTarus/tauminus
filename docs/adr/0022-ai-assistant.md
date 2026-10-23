# ADR 0022: The AI assistant

- Status: accepted
- Date: 2026-10-23

## Context

v0.4 adds an AI assistant: a command for what the user asks, and an
explanation of what went wrong in a pane. It is off by default, takes the
user's own key or a local model, and every command it suggests passes the
prod guard. The exit criterion: it never sends terminal output off the
machine without the host's leave, proven by a test. A panel of two
(security, architecture) weighed the providers, where the calls run, what
a request may hold, and what an answer may do.

## Decisions

### One wire protocol, called from Rust

The assistant speaks the OpenAI-compatible chat completions API. That one
API covers OpenAI, Ollama, llama.cpp's server, LM Studio, vLLM, and the
proxies that put other providers behind it. The `tauminus-ai` adapter
posts with `reqwest` over rustls on ring (already in the tree through
russh) and the OS's certificate checks. It follows no redirect, reads no
proxy from the environment, maps `localhost` to 127.0.0.1 without asking
DNS, times out (10 s to connect, 60 s to answer, 180 s on loopback), and
reads at most 256 KiB of an answer. Errors name the status, never the
body, which may echo the request. `deny.toml` keeps `reqwest` in the
adapter. The webview makes no request: its CSP stays `connect-src` IPC
only, and the key, once typed, is never read back into it.

### The address and the key

The server address must be `https://`, or `http://` to a literal
loopback address or `localhost`. It may hold no user, query or fragment.
`ai.json` keeps the address, the model, the on switch, and which origin
a key was saved for; it holds no secret. The key is a vault record,
written from the view and never read back. It goes to the origin it was
saved for and nowhere else: a new address forgets it, and a key typed
next to an address not saved yet is refused.

### Output goes only with the host's leave, decided in the backend

The view names a session and types requests; no IPC command takes
terminal text. A host or group setting, `aiOutput`, lets the assistant
read what the host printed. It inherits like the history setting but is
off at the root. Local shells have one setting of their own, off by
default. Ad hoc SSH, Telnet and serial sessions never send output.

To explain, the backend reads the last 16 KiB the session showed, from a
ring the pump fills as output arrives (memory only), and only after it
finds the leave. It finds it from the session's recorded saved host
(ADR 0021), and only while the host is still saved with the address the
session logged in to. It strips escapes and keeps what a carriage return
left on each line. It hides secrets of a known shape as the history does
and keeps the last 8 KiB. The user reads that text and clicks Send; the
backend checks the leave again before it sends, and sends only to the
server the preview named. A loopback server needs
the same leave: it may be a tunnel (Tauminus' own forwards included) or
forward to a hosted model.

A command request sends the typed request (at most 2 KiB), the kind of
session, and this computer's OS family for a local shell. It sends no
host name, user, directory or history.

### An answer runs nothing

A model that read what a host printed may answer what that host wants,
so its answer is treated as written by the host. It is shown as plain
text: control and unseen characters out, no markdown. The command it may
offer is the first fenced block, if that is one line of visible
characters. The backend checks it with the host's prod guard and the
built-in rules on, even where the host's guard is off, and the view shows
the findings first. The view puts the command on the line as a paste,
without Enter, and only on the normal screen, with nothing typed on the
line, and at the prompt where the shell marks it (OSC 133). The user's
Enter then passes the pane's own guard as any command does. There is no
run button and no tool use.

## Consequences

- `tests` in `tauminus-app`'s `ai` module prove the exit criterion with a
  transport that records every request: without the leave no request
  holds the session's output, and the cases cover inheritance, a host's
  own no, a leave withdrawn after the preview, an edited or deleted host,
  local, ad hoc, Telnet and serial sessions, and a loopback server.
- A request the user types can still carry output they pasted into it.
- The guard does not flag every harmful command (`curl -d @~/.ssh/id_ed25519`
  reads as harmless); the user reads what goes on the line.
- Script running in the webview could point the assistant at its own
  server and send it what the webview already sees, up to 2 KiB a
  request, or turn a host's leave on. That adds little to what such a
  script already reaches (an ad hoc SSH or Telnet session to its own
  host); the key is never sent there, since a new origin forgets it.
- Redaction hides secrets of a known shape. A bare value with no name
  (a `.pgpass` line, a password hash) and text the terminal hid (SGR 8)
  go as printed; the preview shows them before Send.
- With broadcast input on, a command put on the line goes to every
  linked pane, as a paste does; each pane's own guard checks it on Enter.
- One request per pane at a time; there is no streaming and no chat.
- A server that wants a native API other than OpenAI's needs a proxy, or
  a second adapter behind the same port.
