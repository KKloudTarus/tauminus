// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type SkippedView,
  type SnippetInput,
  type SnippetView,
  type StartupView,
  type VariableView,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { SkippedView, SnippetInput, SnippetView, StartupView, VariableView };

/** The saved snippets, and the text to type to run one. */
export interface SnippetsClient {
  /** By folder, then name. */
  list(): Promise<SnippetView[]>;
  /** Adds or replaces; returns the whole list. */
  save(snippet: SnippetInput): Promise<SnippetView[]>;
  remove(id: string): Promise<SnippetView[]>;
  /** Filled in from `values` and the defaults, each line ended by Enter. */
  command(id: string, values: Record<string, string>): Promise<string>;
  /** What to type into a new shell on a saved host. */
  startup(host: string): Promise<StartupView>;
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

export const tauriSnippets: SnippetsClient = {
  list: () => unwrap(commands.snippetsList()),
  save: (snippet) => unwrap(commands.snippetSave(snippet)),
  remove: (id) => unwrap(commands.snippetDelete(id)),
  command: (id, values) => unwrap(commands.snippetCommand(id, values)),
  startup: (host) => unwrap(commands.hostStartup(host)),
};
