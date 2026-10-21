// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The only entry point to the backend. Lint forbids importing @tauri-apps/*
// anywhere else, so every IPC call goes through a typed port here.
export {
  type ErrorCode,
  type GridSize,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type TerminalSession,
  tauriSessions,
} from "./sessions.ts";
