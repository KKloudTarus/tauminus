// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type ExitFilterSpec,
  type HistoryHitSpec,
  type HistoryQuerySpec,
  type HistoryRecordSpec,
  type RecordedSpec,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { ExitFilterSpec, HistoryHitSpec, HistoryQuerySpec, HistoryRecordSpec, RecordedSpec };

/** The command history. Searching needs the vault open (`vault_locked`). */
export interface HistoryClient {
  record(entry: HistoryRecordSpec): Promise<RecordedSpec>;
  search(query: HistoryQuerySpec): Promise<HistoryHitSpec[]>;
  output(id: number): Promise<string | null>;
  clear(host: string | null): Promise<number>;
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

export const tauriHistory: HistoryClient = {
  record: (entry) => unwrap(commands.historyRecord(entry)),
  search: (query) => unwrap(commands.historySearch(query)),
  output: (id) => unwrap(commands.historyOutput(id)),
  clear: (host) => unwrap(commands.historyClear(host)),
};
