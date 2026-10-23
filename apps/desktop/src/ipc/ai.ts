// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type AiPreviewView,
  type AiReplyView,
  type AiSettingsSpec,
  type AiSettingsView,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { AiPreviewView, AiReplyView, AiSettingsSpec, AiSettingsView };

/**
 * The AI assistant. Every call names a session, never what it printed: the
 * backend reads a session's output itself, and only where its host lets
 * the assistant read it (`not_allowed` otherwise). The API key goes in and
 * never comes back.
 */
export interface AiClient {
  settings(): Promise<AiSettingsView>;
  /** A new server address forgets the saved key. */
  saveSettings(settings: AiSettingsSpec): Promise<AiSettingsView>;
  /**
   * Kept in the vault, for the server's address only (`vault_locked`).
   * `baseUrl` is the address shown with it; it must be the saved one.
   */
  saveKey(key: string, baseUrl: string): Promise<AiSettingsView>;
  clearKey(): Promise<AiSettingsView>;
  /** A command for `request`, typed in `session`. Sends nothing it printed. */
  ask(session: number, request: string): Promise<AiReplyView>;
  /** What explaining `session`'s output would send, to read first. */
  preview(session: number): Promise<AiPreviewView>;
  /** Sends what `preview` showed. */
  explain(session: number, preview: number): Promise<AiReplyView>;
}

type Result<T> = Promise<
  | { status: "ok"; data: T }
  | { status: "error"; error: { code: SessionCommandError["code"]; message: string } }
>;

async function unwrap<T>(result: Result<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") throw new SessionCommandError(outcome.error);
  return outcome.data;
}

export const tauriAi: AiClient = {
  settings: () => unwrap(commands.aiSettingsGet()),
  saveSettings: (settings) => unwrap(commands.aiSettingsSet(settings)),
  saveKey: (key, baseUrl) => unwrap(commands.aiKeySet(key, baseUrl)),
  clearKey: () => unwrap(commands.aiKeyClear()),
  ask: (session, request) => unwrap(commands.aiAsk(session, request)),
  preview: (session) => unwrap(commands.aiPreview(session)),
  explain: (session, preview) => unwrap(commands.aiExplain(session, preview)),
};
