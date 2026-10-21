// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import {
  type CommandError,
  type ErrorCode,
  type GridSize,
  type SessionExit,
  commands,
} from "./bindings.ts";

export type { ErrorCode, GridSize };

/** A failed session command, carrying the stable code from the backend. */
export class SessionCommandError extends Error {
  readonly code: ErrorCode;

  constructor(error: CommandError) {
    super(error.message);
    this.name = "SessionCommandError";
    this.code = error.code;
  }
}

export interface SessionEvents {
  /** Called with each batch of terminal output, in order. */
  onOutput(data: Uint8Array): void;
  /** Called once when the session ends. */
  onExit(exitCode: number | null): void;
}

export interface TerminalSession {
  readonly id: number;
  write(data: string): Promise<void>;
  /** Bytes that are not valid UTF-8, such as legacy mouse reports. */
  writeBytes(data: Uint8Array): Promise<void>;
  resize(size: GridSize): Promise<void>;
  /** Reports output bytes the terminal has parsed, so the backend can resume. */
  acknowledge(bytes: number): Promise<void>;
  close(): Promise<void>;
}

/** Opens terminal sessions. Components depend on this, never on Tauri. */
export interface SessionClient {
  open(size: GridSize, events: SessionEvents): Promise<TerminalSession>;
}

type CommandResult<T> = Promise<
  { status: "ok"; data: T } | { status: "error"; error: CommandError }
>;

async function unwrap<T>(result: CommandResult<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") {
    throw new SessionCommandError(outcome.error);
  }
  return outcome.data;
}

async function done(result: CommandResult<null>): Promise<void> {
  await unwrap(result);
}

export const tauriSessions: SessionClient = {
  async open(size, events) {
    const output = new Channel<ArrayBuffer>();
    output.onmessage = (buffer) => events.onOutput(new Uint8Array(buffer));
    const exit = new Channel<SessionExit>();
    exit.onmessage = (event) => events.onExit(event.exitCode);

    const id = await unwrap(commands.sessionOpen(size, output, exit));
    return {
      id,
      write: (data) => done(commands.sessionWrite(id, data)),
      writeBytes: (data) => done(commands.sessionWriteBytes(id, [...data])),
      resize: (next) => done(commands.sessionResize(id, next)),
      acknowledge: (bytes) => done(commands.sessionAcknowledge(id, bytes)),
      close: () => done(commands.sessionClose(id)),
    };
  },
};
