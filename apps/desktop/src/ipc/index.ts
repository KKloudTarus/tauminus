// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The only entry point to the backend. Lint forbids importing @tauri-apps/*
// anywhere else, so every IPC call goes through a typed port here.
import type { HostsClient } from "./hosts.ts";
import type { SessionClient } from "./sessions.ts";

export {
  type GroupInput,
  type GroupView,
  type HostInput,
  type HostView,
  type HostsClient,
  type HostsView,
  type ImportResult,
  type SettingsSpec,
  tauriHosts,
} from "./hosts.ts";
export {
  type AnswerSpec,
  type EndpointSpec,
  type ErrorCode,
  type GridSize,
  type PromptHandler,
  type PromptKind,
  type RouteSpec,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type SessionSpec,
  type SshTargetSpec,
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

/** The hosts client for this build; the `e2e` build keeps hosts in memory. */
export async function hostsClient(): Promise<HostsClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-hosts.ts")).fakeHosts();
  }
  return (await import("./hosts.ts")).tauriHosts;
}
