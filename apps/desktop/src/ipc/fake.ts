// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// An in-memory shell for end-to-end tests of the UI in a plain browser. Only
// the `e2e` build mode loads this module; production bundles do not contain it.

import type { CommandError, ZmodemView } from "./bindings.ts";
import type { ZmodemClient } from "./zmodem.ts";
import {
  type GridSize,
  type LocalShellView,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type SessionSpec,
  type TerminalSession,
  type TmuxClient,
  type TopologyView,
} from "./sessions.ts";

const encoder = new TextEncoder();
const PROMPT = "$ ";

/**
 * Lines it understands: `echo <text>`, `title <text>`, `exit [code]`,
 * `size`, `integrate`, after which prompts carry OSC 133 marks,
 * `later <text>`, which prints after 1 s, `sudo`, which reads a
 * password without echo, `ask <question>`, which waits on the question's
 * line for an answer and prints it, and `drop`, which loses the
 * connection. Ctrl+C drops the line.
 */
/** What fake sessions print, by session, for the fake recordings. */
export const fakeOutputTaps = new Map<number, (text: string) => void>();

/** The Zmodem handler, and each fake shell waiting in `sz` or `rz`. */
let receiveHandler: ((event: ZmodemView) => void) | undefined;
const offers = new Map<number, { name: string; done: () => void }>();

/**
 * Answers as the backend would. `sz NAME`: `NAME` comes, 3 MB, unless the
 * user declines. `rz`: the "dialog" picks `notes.txt` and it goes.
 */
export const fakeZmodem: ZmodemClient = {
  async listen(handler) {
    receiveHandler = handler;
  },
  async choose(session) {
    const offer = offers.get(session);
    if (!offer) return false;
    offers.delete(session);
    receiveHandler?.({ kind: "started", session, folder: null });
    receiveHandler?.({ kind: "file", session, name: "notes.txt", size: 2048 });
    setTimeout(() => {
      receiveHandler?.({
        kind: "finished",
        session,
        folder: null,
        files: ["notes.txt"],
        moreFiles: 0,
        skipped: [],
        moreSkipped: 0,
      });
      offer.done();
    }, 300);
    return true;
  },
  async receive(session, _offer, accept) {
    const offer = offers.get(session);
    if (!offer) return;
    offers.delete(session);
    if (!accept) {
      receiveHandler?.({ kind: "declined", session });
      offer.done();
      return;
    }
    const folder = "/home/me/Downloads/Tauminus";
    receiveHandler?.({ kind: "started", session, folder });
    receiveHandler?.({ kind: "file", session, name: offer.name, size: 3_000_000 });
    receiveHandler?.({ kind: "progress", session, bytes: 1_500_000, size: 3_000_000 });
    setTimeout(() => {
      receiveHandler?.({
        kind: "finished",
        session,
        folder,
        files: [offer.name],
        moreFiles: 0,
        skipped: [],
        moreSkipped: 0,
      });
      offer.done();
    }, 300);
  },
  async cancel(session) {
    const offer = offers.get(session);
    offers.delete(session);
    receiveHandler?.(
      offer ? { kind: "declined", session } : { kind: "failed", session, reason: "cancelled" },
    );
    offer?.done();
  },
  async reveal() {},
};

class FakeShell implements TerminalSession {
  readonly id: number;
  readonly #events: SessionEvents;
  #line = "";
  /** Reading a password after `sudo`: keys do not echo. */
  #secret = false;
  /** Waiting on a question's line for its answer, after `ask`. */
  #asking = false;
  #size: GridSize;
  #ended = false;
  /** After `integrate`: prompts carry OSC 133 marks, as bash's integration. */
  #marked = false;

  get #prompt(): string {
    return this.#marked ? `\u001b]133;A\u0007${PROMPT}\u001b]133;B\u0007` : PROMPT;
  }

  constructor(id: number, size: GridSize, events: SessionEvents, banner = "") {
    this.id = id;
    this.#size = size;
    this.#events = events;
    queueMicrotask(() => this.#print(banner + this.#prompt));
  }

  async write(data: string): Promise<void> {
    for (const char of data) {
      if (this.#ended) return;
      if (char === "\r" && this.#secret) {
        this.#secret = false;
        this.#line = "";
        this.#print(`\r\nsudo: ok\r\n${this.#prompt}`);
      } else if (char === "\r" && this.#asking) {
        this.#asking = false;
        this.#print(`\r\nanswer: ${this.#line}\r\n${this.#prompt}`);
        this.#line = "";
      } else if (char === "\r") {
        this.#print(this.#marked ? "\r\n\u001b]133;C\u0007" : "\r\n");
        this.#run(this.#line.trim());
        this.#line = "";
      } else if (char === "\u0003") {
        // As a shell does: drop the line and show a new prompt.
        this.#line = "";
        this.#print(`^C\r\n${this.#prompt}`);
      } else if (char === "\u007f") {
        if (this.#line.length > 0) {
          this.#line = this.#line.slice(0, -1);
          this.#print("\b \b");
        }
      } else if (char >= " ") {
        this.#line += char;
        if (!this.#secret) this.#print(char);
      }
    }
  }

  async writeBytes(): Promise<void> {}

  async resize(size: GridSize): Promise<void> {
    this.#size = size;
  }

  async acknowledge(): Promise<void> {}

  async probe(): Promise<void> {}

  async close(): Promise<void> {
    this.#exit(null);
  }

  #run(line: string) {
    const [command = "", ...rest] = line.split(" ");
    const argument = rest.join(" ");
    switch (command) {
      case "":
        break;
      case "ask":
        this.#asking = true;
        this.#print(`${argument} `);
        return;
      case "sudo":
        this.#secret = true;
        this.#print("[sudo] password for me: ");
        return;
      case "later":
        // Output that arrives a moment after the command, as from a build.
        setTimeout(() => this.#print(`${argument}\r\n${this.#prompt}`), 1000);
        return;
      case "echo":
        this.#print(`${argument}\r\n`);
        break;
      case "fail":
        // Reports its exit status as shells with OSC 133 integration do.
        this.#print(`failed: ${argument}\r\n\u001b]133;D;2\u0007`);
        break;
      case "integrate":
        this.#marked = true;
        break;
      case "title":
        this.#print(`\u001b]0;${argument}\u0007`);
        break;
      case "size":
        this.#print(`${this.#size.cols}x${this.#size.rows}\r\n`);
        break;
      case "exit":
        this.#exit(Number.parseInt(argument || "0", 10));
        return;
      case "sz":
        // As `sz` does: it asks the terminal to receive, then waits.
        this.#print("rz\r");
        offers.set(this.id, {
          name: argument || "file.bin",
          done: () => this.#print(`\r\n${this.#prompt}`),
        });
        receiveHandler?.({
          kind: "offered",
          session: this.id,
          offer: 1,
          wants: "receive",
          protocol: "zmodem",
        });
        return;
      case "tsz":
        // As trzsz's `tsz` does: its trigger line, then it waits.
        this.#print("\x1b[s");
        offers.set(this.id, {
          name: argument || "file.bin",
          done: () => this.#print(`\r\n${this.#prompt}`),
        });
        receiveHandler?.({
          kind: "offered",
          session: this.id,
          offer: 1,
          wants: "receive",
          protocol: "trzsz",
        });
        return;
      case "rz":
        // As `rz` does: it says it waits, then waits for files.
        this.#print("rz waiting to receive.");
        offers.set(this.id, {
          name: "",
          done: () => this.#print(`\r\n${this.#prompt}`),
        });
        receiveHandler?.({
          kind: "offered",
          session: this.id,
          offer: 1,
          wants: "send",
          protocol: "zmodem",
        });
        return;
      case "drop":
        // As a network going away: the session ends without an exit.
        this.#exit(null, true);
        return;
      default:
        this.#print(`${command}: command not found\r\n`);
    }
    this.#print(this.#prompt);
  }

  #print(text: string) {
    fakeOutputTaps.get(this.id)?.(text);
    const bytes = encoder.encode(text);
    this.#emitted += bytes.length;
    this.#events.onOutput(bytes);
  }

  #emitted = 0;
  /** The fake vault's identity logs in to it: `vault.test`. */
  vaultHost = false;

  async fillOffer(): Promise<{ identity: string | null; login: string } | null> {
    if (!this.vaultHost) return null;
    const { sharedFakeVault } = await import("./fake-vault.ts");
    return { identity: sharedFakeVault().unlocked() ? "deploy" : null, login: "me@vault.test" };
  }

  /** Types the fake identity's password as keys: echoed unless a prompt hides it. */
  async fillPassword(seen: number): Promise<void> {
    if (!this.vaultHost) throw fail("not_found", "this session is not a saved host's");
    const { sharedFakeVault } = await import("./fake-vault.ts");
    if (!sharedFakeVault().unlocked()) throw fail("vault_locked", "the vault is locked");
    if (this.#emitted > seen) throw fail("cancelled", "the session showed more since");
    await this.write(FAKE_IDENTITY_PASSWORD);
  }

  #exit(code: number | null, lost = false) {
    if (this.#ended) return;
    this.#ended = true;
    queueMicrotask(() => this.#events.onExit(code, lost));
  }
}

let nextId = 1;

/** The password every fake SSH host accepts. */
export const FAKE_PASSWORD = "secret";
/** The fake vault identity's password, which `fillPassword` types. */
export const FAKE_IDENTITY_PASSWORD = "hunter2";
const FAKE_FINGERPRINT = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";

function fail(code: CommandError["code"], message: string): SessionCommandError {
  return new SessionCommandError({ code, message });
}

/**
 * Hosts by name: `prompt.test` asks to trust its key and for a password;
 * `kbd.test` asks a keyboard-interactive code (`123456`); `changed.test`
 * has a changed host key; `vault.test` signs in with an identity, so it
 * needs the vault unlocked; anything else cannot be reached.
 */
async function fakeSsh(
  spec: Exclude<
    SessionSpec,
    | { kind: "local" }
    | { kind: "telnet" }
    | { kind: "mosh" }
    | { kind: "serial" }
    | { kind: "tmux" }
  >,
  events: SessionEvents,
): Promise<string> {
  const ask = events.onPrompt ?? (async () => ({ kind: "cancel" }) as const);
  const { host, port } = spec.target.destination;
  const endpoint = `${spec.target.destination.user || "me"}@${host}`;
  if (host === "changed.test") {
    throw fail(
      "host_key_changed",
      `the host key for ${host}:${port} has changed (recorded at ~/.ssh/known_hosts:3)`,
    );
  }
  if (host === "vault.test") {
    const { sharedFakeVault } = await import("./fake-vault.ts");
    if (!sharedFakeVault().unlocked()) throw fail("vault_locked", "the vault is locked");
    return `Welcome to ${host}, signed in from the vault\r\n`;
  }
  if (host !== "prompt.test" && host !== "kbd.test") {
    throw fail("connect_failed", `could not resolve ${host}`);
  }
  const trust = await ask({
    kind: "unknownHostKey",
    host,
    port,
    algorithm: "ssh-ed25519",
    fingerprint: FAKE_FINGERPRINT,
    certificateExpected: false,
  });
  if (trust.kind !== "trustHostKey")
    throw fail("cancelled", `the host key for ${host}:${port} was not trusted`);
  if (host === "kbd.test") {
    const answer = await ask({
      kind: "keyboardInteractive",
      endpoint,
      name: "Two-factor",
      instructions: "Enter the code from your app",
      fields: [{ text: "Code: ", echo: true }],
    });
    if (answer.kind !== "responses" || answer.values[0] !== "123456") {
      throw fail(
        "auth_failed",
        `authentication to ${endpoint} failed after trying keyboard-interactive`,
      );
    }
    return `Welcome to ${host}\r\n`;
  }
  for (let attempt = 0; attempt < 3; attempt += 1) {
    // oxlint-disable-next-line no-await-in-loop -- each attempt waits for the user's answer
    const answer = await ask({ kind: "password", endpoint });
    if (answer.kind !== "secret") throw fail("cancelled", "cancelled");
    if (answer.value === FAKE_PASSWORD) return `Welcome to ${host}\r\n`;
  }
  throw fail("auth_failed", `authentication to ${endpoint} failed after trying password`);
}

/**
 * A tmux session as tabs, for a host whose address is `tmux.test`: two
 * windows, `shell` and `logs`; any other host's tmux is too old, and it
 * is joined the plain way.
 */
async function fakeTmuxBanner(
  spec: Extract<SessionSpec, { kind: "tmux" }>,
  events: SessionEvents,
): Promise<string> {
  if (spec.pane !== undefined) return `tmux pane %${spec.pane}\r\n`;
  const { fakeHostAddress } = await import("./fake-hosts.ts");
  if (fakeHostAddress(spec.hostId) !== "tmux.test") {
    events.tmux?.fellBack(
      "tmux 3.1c is too old to show its windows as tabs: 3.2 or later is needed",
    );
    return `Connected to ${spec.label}, in tmux\r\n`;
  }
  tmuxControls += 1;
  events.tmux?.opened(tmuxControls, { window: 1, pane: 1, name: "shell" }, [
    { window: 2, pane: 2, name: "logs" },
  ]);
  return "tmux pane %1\r\n";
}

let tmuxControls = 0;
let tmuxWindows = 2;
/** The topology handler of each fake control client, for new windows. */
const tmuxTopologies = new Map<number, (event: TopologyView) => void>();

export const fakeTmux: TmuxClient = {
  async newWindow(control) {
    tmuxWindows += 1;
    tmuxTopologies.get(control)?.({
      kind: "window",
      window: tmuxWindows,
      pane: tmuxWindows,
      name: "new",
    });
  },
  async detach(control) {
    tmuxTopologies.get(control)?.({ kind: "ended", reason: "detached", lost: false });
  },
};

export const fakeSessions: SessionClient = {
  async open(spec, size, events) {
    if (spec.kind === "tmux" && spec.control === undefined && events.tmux) {
      const tmux = events.tmux;
      tmuxTopologies.set(tmuxControls + 1, (event) => tmux.topology(event));
    }
    if (spec.kind === "local" && spec.shell && !FAKE_SHELLS.some((s) => s.id === spec.shell?.id)) {
      throw fail("not_found", `${spec.shell.id} is not a shell on this computer`);
    }
    const banner =
      spec.kind === "tmux"
        ? await fakeTmuxBanner(spec, events)
        : spec.kind === "local"
          ? spec.shell
            ? `${spec.shell.name}\r\n`
            : ""
          : spec.kind === "telnet"
            ? `Connected to ${spec.label} over Telnet\r\n`
            : spec.kind === "mosh"
              ? `Connected to ${spec.label} over Mosh\r\n`
              : spec.kind === "serial"
                ? `Opened ${spec.port.path} at ${spec.port.baud} baud\r\n`
                : await fakeSsh(spec, events);
    const id = nextId;
    nextId += 1;
    const shell = new FakeShell(id, size, events, banner);
    shell.vaultHost =
      (spec.kind === "host" || spec.kind === "ssh") &&
      spec.target.destination.host === "vault.test";
    return shell;
  },
  localShells: async () => ({
    shells: FAKE_SHELLS.map(({ id, name }) => ({ id, name })),
    complete: true,
  }),
};

/** The shells the fake computer has besides the default; a local tab in one prints its name. */
export const FAKE_SHELLS: readonly LocalShellView[] = [
  { id: "pwsh", name: "PowerShell 7" },
  { id: "wsl:Ubuntu", name: "Ubuntu (WSL)" },
];
