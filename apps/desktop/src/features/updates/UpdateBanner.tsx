// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useSyncExternalStore } from "react";

import { Icon } from "@/app/icons.tsx";

import { type UpdatesStore, updateNotice } from "./updates-store.ts";

/**
 * One line under the tabs while there is something to say about updates:
 * a version to install, or the answer to a check the user asked for.
 */
export function UpdateBanner({ store }: { store: UpdatesStore }) {
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const text = updateNotice(state);
  if (text === undefined) return null;
  const notes = state.status === "available" ? state.update?.notes.trim() : undefined;
  return (
    <output
      aria-label="Updates"
      className="mx-2 my-1 flex items-start gap-2 rounded-lg border border-line bg-surface-2 py-1.5 pr-1 pl-2.5 text-xs"
    >
      <div className="flex-1 pt-1">
        <span>{text}</span>
        {notes && (
          <details className="mt-1">
            <summary>What changed</summary>
            <p className="mt-1 whitespace-pre-wrap">{notes}</p>
          </details>
        )}
      </div>
      {state.status === "available" && (
        <button type="button" className="btn btn-primary" onClick={() => void store.install()}>
          Install and restart
        </button>
      )}
      {state.status !== "installing" && (
        <button
          type="button"
          onClick={() => store.dismiss()}
          aria-label={state.status === "available" ? "Later" : "Dismiss"}
          title={state.status === "available" ? "Later" : "Dismiss"}
          className="icon-btn size-6 pointer-coarse:size-10"
        >
          <Icon name="x" className="size-3.5" />
        </button>
      )}
    </output>
  );
}
