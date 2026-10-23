// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// An in-memory shell for end-to-end tests of the UI in a plain browser. Only
// the `e2e` build mode loads this module; production bundles do not contain it.

import type { CommandError } from "./bindings.ts";
import {
  type GridSize,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type SessionSpec,
  type TerminalSession,
} from "./sessions.ts";

const encoder = new TextEncoder();
const PROMPT = "$ ";

/**
 * Lines it understands: `echo <text>`, `title <text>`, `exit [code]`,
 * `size`, `later <text>`, which prints after 1 s, `sudo`, which reads a
 * password without echo, and `drop`, which loses the connection. Ctrl+C drops
 * the line.
 */
class FakeShell implements TerminalSession {
  readonly id: number;
  readonly #events: SessionEvents;
  #line = "";
  /** Reading a password after `sudo`: keys do not echo. */
  #secret = false;
  #size: GridSize;
  #ended = false;

  constructor(id: number, size: GridSize, events: SessionEvents, banner = "") {
    this.id = id;
    this.#size = size;
    this.#events = events;
    queueMicrotask(() => this.#print(banner + PROMPT));
  }

  async write(data: string): Promise<void> {
    for (const char of data) {
      if (this.#ended) return;
      if (char === "\r" && this.#secret) {
        this.#secret = false;
        this.#line = "";
        this.#print(`\r\nsudo: ok\r\n${PROMPT}`);
      } else if (char === "\r") {
        this.#print("\r\n");
        this.#run(this.#line.trim());
        this.#line = "";
      } else if (char === "\u0003") {
        // As a shell does: drop the line and show a new prompt.
        this.#line = "";
        this.#print(`^C\r\n${PROMPT}`);
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
      case "sudo":
        this.#secret = true;
        this.#print("[sudo] password for me: ");
        return;
      case "later":
        // Output that arrives a moment after the command, as from a build.
        setTimeout(() => this.#print(`${argument}\r\n${PROMPT}`), 1000);
        return;
      case "echo":
        this.#print(`${argument}\r\n`);
        break;
      case "fail":
        // Reports its exit status as shells with OSC 133 integration do.
        this.#print(`failed: ${argument}\r\n\u001b]133;D;2\u0007`);
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
      case "drop":
        // As a network going away: the session ends without an exit.
        this.#exit(null, true);
        return;
      default:
        this.#print(`${command}: command not found\r\n`);
    }
    this.#print(PROMPT);
  }

  #print(text: string) {
    this.#events.onOutput(encoder.encode(text));
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
  spec: Exclude<SessionSpec, { kind: "local" } | { kind: "telnet" } | { kind: "serial" }>,
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

export const fakeSessions: SessionClient = {
  async open(spec, size, events) {
    const banner =
      spec.kind === "local"
        ? ""
        : spec.kind === "telnet"
          ? `Connected to ${spec.label} over Telnet\r\n`
          : spec.kind === "serial"
            ? `Opened ${spec.port.path} at ${spec.port.baud} baud\r\n`
            : await fakeSsh(spec, events);
    const id = nextId;
    nextId += 1;
    return new FakeShell(id, size, events, banner);
  },
};
