// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The only entry point to the backend. Lint forbids importing @tauri-apps/*
// anywhere else, so every IPC call goes through a typed port here.
import type { SessionClient } from "./sessions.ts";

export {
  type ErrorCode,
  type GridSize,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type TerminalSession,
  tauriSessions,
} from "./sessions.ts";

/**
 * The session client for this build. Vite replaces `import.meta.env.MODE` at
 * build time, so production bundles drop the fake shell entirely.
 */
export async function sessionClient(): Promise<SessionClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake.ts")).fakeSessions;
  }
  return (await import("./sessions.ts")).tauriSessions;
}
