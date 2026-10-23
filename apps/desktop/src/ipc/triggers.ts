// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type ActionSpec,
  type FiredView,
  type HitView,
  type PreparedView,
  type RuleSpec,
  type StyleSpec,
  type TriggerSetInput,
  type TriggerSetView,
  type TriggerView,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type {
  ActionSpec,
  FiredView,
  HitView,
  PreparedView,
  RuleSpec,
  StyleSpec,
  TriggerSetInput,
  TriggerSetView,
  TriggerView,
};

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
  /** The triggers finished lines of output fire; `null` when stale. */
  fire(handle: number, lines: string[]): Promise<FiredView[] | null>;
  /**
   * A system notification for notifying trigger `rule`, made by the
   * backend from the rule's name and the host's label; `false` when none
   * showed (the window has focus, or the rule notified just now).
   */
  notify(host: string | null, handle: number, rule: number): Promise<boolean>;
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
  fire: (handle, lines) => unwrap(commands.triggerFire(handle, lines)),
  notify: (host, handle, rule) => unwrap(commands.triggerNotify(host, handle, rule)),
};
