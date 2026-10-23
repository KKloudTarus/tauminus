// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useId, useMemo, useState } from "react";

import { MAX_PANES_PER_TAB } from "@/features/workspace/workspace.ts";
import type { HostView, SnippetView } from "@/ipc/index.ts";

import { snippetTitle } from "./snippets-store.ts";

/** Rows shown for a search; more would only slow typing down. */
const MAX_SHOWN = 200;

/**
 * Picks the saved hosts a snippet runs on, one pane each in a new tab, at
 * most a tab's worth. O(H) per keystroke.
 */
export function SnippetHosts({
  snippet,
  hosts,
  onRun,
  onClose,
}: {
  snippet: SnippetView;
  hosts: readonly HostView[];
  onRun: (hosts: readonly string[]) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const [query, setQuery] = useState("");
  const [chosen, setChosen] = useState<readonly string[]>([]);
  const shown = useMemo(() => {
    const words = query.toLowerCase().split(/\s+/u).filter(Boolean);
    return hosts
      .filter((h) => {
        const text = `${h.label} ${h.address} ${h.tags.join(" ")}`.toLowerCase();
        return words.every((w) => text.includes(w));
      })
      .slice(0, MAX_SHOWN);
  }, [hosts, query]);
  const full = chosen.length >= MAX_PANES_PER_TAB;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (chosen.length > 0) onRun(chosen);
  };
  return (
    <div className="fixed inset-0 z-30 flex items-start justify-center bg-black/30 pt-[10vh]">
      <dialog
        open
        aria-labelledby={titleId}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        className="static m-0 max-h-[75vh] w-[min(520px,92vw)] overflow-y-auto rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl"
      >
        <h2 id={titleId} className="mb-3 text-base font-medium">
          Run {snippetTitle(snippet)} on hosts
        </h2>
        <form onSubmit={submit} className="flex flex-col gap-2">
          <input
            aria-label="Find hosts"
            placeholder="Name, address, or tag"
            value={query}
            spellCheck={false}
            onChange={(e) => setQuery(e.target.value)}
            className="w-full rounded border border-current/20 bg-transparent px-2 py-1 outline-none focus:border-current/50"
          />
          <ul aria-label="Hosts" className="flex flex-col">
            {shown.map((host) => {
              const on = chosen.includes(host.id);
              return (
                <li key={host.id}>
                  <label className="flex items-center gap-2 py-0.5">
                    <input
                      type="checkbox"
                      checked={on}
                      disabled={!on && (full || !host.target)}
                      onChange={() =>
                        setChosen((all) =>
                          on ? all.filter((id) => id !== host.id) : [...all, host.id],
                        )
                      }
                    />
                    <span className="truncate">{host.label}</span>
                    <span className="truncate text-xs opacity-60">
                      {host.target ? host.address : (host.problem ?? "cannot connect")}
                    </span>
                  </label>
                </li>
              );
            })}
          </ul>
          <p className="text-xs opacity-70">
            Each host opens in its own pane of a new tab, and they broadcast together.{" "}
            {full ? `A tab holds at most ${MAX_PANES_PER_TAB}.` : ""}
          </p>
          <div className="flex justify-end gap-2">
            <button type="button" className="rounded px-2 hover:bg-current/10" onClick={onClose}>
              Cancel
            </button>
            <button
              type="submit"
              disabled={chosen.length === 0}
              className="rounded px-2 hover:bg-current/10 disabled:opacity-40"
            >
              Run on {chosen.length} {chosen.length === 1 ? "host" : "hosts"}
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}
