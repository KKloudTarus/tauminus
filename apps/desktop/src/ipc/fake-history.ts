// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The history in memory for end-to-end tests, following the shared fake
// vault: searching needs it open, as the real one does.

import { sharedFakeVault } from "./fake-vault.ts";
import type { HistoryClient, HistoryHitSpec } from "./history.ts";
import { SessionCommandError } from "./sessions.ts";

interface Row extends HistoryHitSpec {
  output: string;
}

function open(): void {
  if (!sharedFakeVault().unlocked()) {
    throw new SessionCommandError({
      code: "vault_locked",
      message: "unlock the vault to search the history",
    });
  }
}

/** The output with the first word found marked, as the real search marks it. */
function mark(output: string, words: readonly string[]): string {
  const lower = output.toLowerCase();
  const word = words.find((w) => lower.includes(w));
  if (!word) return "";
  const at = lower.indexOf(word);
  const end = at + word.length;
  return `${output.slice(0, at)}\u0001${output.slice(at, end)}\u0002${output.slice(end)}`;
}

export function fakeHistory(): HistoryClient {
  const rows: Row[] = [];
  let next = 1;
  return {
    async record(entry) {
      if (!entry.command.trim()) return "dropped";
      rows.push({
        id: next++,
        host: entry.host ?? null,
        label: entry.label,
        at: Math.floor(Date.now() / 1000),
        exit: entry.exit ?? null,
        command: entry.command.trim(),
        output: entry.output ?? "",
        snippet: "",
      });
      return sharedFakeVault().unlocked() ? "stored" : "sealed";
    },
    async search(query) {
      open();
      const words = (query.text ?? "").toLowerCase().split(/\s+/u).filter(Boolean);
      return rows
        .filter((r) => words.every((w) => `${r.command}\n${r.output}`.toLowerCase().includes(w)))
        .filter((r) => query.host == null || (r.host ?? "") === query.host)
        .filter(
          (r) =>
            query.exit == null ||
            query.exit === "any" ||
            (query.exit === "success" ? r.exit === 0 : r.exit !== null && r.exit !== 0),
        )
        .toReversed()
        .slice(0, query.limit)
        .map((row): HistoryHitSpec => ({
          id: row.id,
          host: row.host,
          label: row.label,
          at: row.at,
          exit: row.exit,
          command: row.command,
          snippet: mark(row.output, words),
        }));
    },
    async output(id) {
      open();
      return rows.find((r) => r.id === id)?.output ?? null;
    },
    async clear(host) {
      open();
      const before = rows.length;
      for (let i = rows.length - 1; i >= 0; i -= 1) {
        if (host === null || (rows[i]?.host ?? "") === host) rows.splice(i, 1);
      }
      return before - rows.length;
    },
  };
}
