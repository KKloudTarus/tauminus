// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type HitView,
  type PreparedView,
  type RuleSpec,
  type StyleSpec,
  type TriggerSetInput,
  type TriggerSetView,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { HitView, PreparedView, RuleSpec, StyleSpec, TriggerSetInput, TriggerSetView };

/** Highlight sets: the library, and matching what a pane shows. */
export interface TriggersClient {
  /** By name. */
  list(): Promise<TriggerSetView[]>;
  /** Adds or replaces; returns the whole list. */
  save(set: TriggerSetInput): Promise<TriggerSetView[]>;
  remove(id: string): Promise<TriggerSetView[]>;
  /** The sets every session gets, in precedence order. */
  saveDefaults(ids: string[]): Promise<TriggerSetView[]>;
  /** Why `pattern` cannot be a rule's, or `null`. */
  check(pattern: string): Promise<string | null>;
  /** Compiles the rules of saved host `host`, or only the defaults. */
  prepare(host: string | null): Promise<PreparedView>;
  /** `null` when the rules changed since `prepare`. */
  scan(handle: number, lines: string[]): Promise<HitView[] | null>;
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

export const tauriTriggers: TriggersClient = {
  list: () => unwrap(commands.triggerSetsList()),
  save: (set) => unwrap(commands.triggerSetSave(set)),
  remove: (id) => unwrap(commands.triggerSetDelete(id)),
  saveDefaults: (ids) => unwrap(commands.triggerDefaultsSave(ids)),
  check: (pattern) => commands.triggerPatternCheck(pattern),
  prepare: (host) => unwrap(commands.triggerPrepare(host)),
  scan: (handle, lines) => unwrap(commands.triggerScan(handle, lines)),
};
