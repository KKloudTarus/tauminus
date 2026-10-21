// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// An in-memory shell for end-to-end tests of the UI in a plain browser. Only
// the `e2e` build mode loads this module; production bundles do not contain it.

import type { GridSize, SessionClient, SessionEvents, TerminalSession } from "./sessions.ts";

const encoder = new TextEncoder();
const PROMPT = "$ ";

/** Lines it understands: `echo <text>`, `title <text>`, `exit [code]`, `size`. */
class FakeShell implements TerminalSession {
  readonly id: number;
  readonly #events: SessionEvents;
  #line = "";
  #size: GridSize;
  #ended = false;

  constructor(id: number, size: GridSize, events: SessionEvents) {
    this.id = id;
    this.#size = size;
    this.#events = events;
    queueMicrotask(() => this.#print(PROMPT));
  }

  async write(data: string): Promise<void> {
    for (const char of data) {
      if (this.#ended) return;
      if (char === "\r") {
        this.#print("\r\n");
        this.#run(this.#line.trim());
        this.#line = "";
      } else if (char === "\u007f") {
        if (this.#line.length > 0) {
          this.#line = this.#line.slice(0, -1);
          this.#print("\b \b");
        }
      } else if (char >= " ") {
        this.#line += char;
        this.#print(char);
      }
    }
  }

  async writeBytes(): Promise<void> {}

  async resize(size: GridSize): Promise<void> {
    this.#size = size;
  }

  async acknowledge(): Promise<void> {}

  async close(): Promise<void> {
    this.#exit(null);
  }

  #run(line: string) {
    const [command = "", ...rest] = line.split(" ");
    const argument = rest.join(" ");
    switch (command) {
      case "":
        break;
      case "echo":
        this.#print(`${argument}\r\n`);
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
      default:
        this.#print(`${command}: command not found\r\n`);
    }
    this.#print(PROMPT);
  }

  #print(text: string) {
    this.#events.onOutput(encoder.encode(text));
  }

  #exit(code: number | null) {
    if (this.#ended) return;
    this.#ended = true;
    queueMicrotask(() => this.#events.onExit(code));
  }
}

let nextId = 1;

export const fakeSessions: SessionClient = {
  async open(size, events) {
    const id = nextId;
    nextId += 1;
    return new FakeShell(id, size, events);
  },
};
