// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";

import { templatePanes } from "./layout.ts";
import { type SavedWorkspaces, directConnections } from "./saved.ts";

const BUTTON = "btn btn-ghost btn-xs";

/** Saves the open tabs under a name, and opens or deletes saved sets. */
export function WorkspacesDialog({
  workspaces,
  onSave,
  onOpen,
  onClose,
}: {
  workspaces: SavedWorkspaces;
  /** Returns what to tell the user, or undefined when it saved quietly. */
  onSave: (name: string) => string | undefined;
  onOpen: (id: string) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const all = useSyncExternalStore(workspaces.subscribe, workspaces.getSnapshot);
  const dialog = useRef<HTMLDialogElement>(null);
  const [name, setName] = useState("");
  const [note, setNote] = useState<string | undefined>();
  useEffect(() => dialog.current?.focus(), []);
  const replaces = all.some((w) => w.name === name.trim());
  const save = (event: FormEvent) => {
    event.preventDefault();
    setNote(onSave(name) ?? `Saved ${name.trim()}`);
    setName("");
  };
  return (
    <div className="overlay">
      <dialog
        ref={dialog}
        open
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        className="panel static m-0 max-h-[75vh] w-[min(520px,92vw)] overflow-y-auto p-4 outline-none"
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 id={titleId} className="text-base font-medium">
            Workspaces
          </h2>
          <button type="button" className={BUTTON} onClick={onClose}>
            Close
          </button>
        </div>
        <form onSubmit={save} className="mb-3 flex items-center gap-2">
          <input
            aria-label="Workspace name"
            placeholder="Name for the open tabs"
            value={name}
            maxLength={128}
            onChange={(e) => setName(e.target.value)}
            className="field min-w-0 flex-1"
          />
          <button type="submit" className={BUTTON} disabled={!name.trim()}>
            {replaces ? "Replace" : "Save tabs"}
          </button>
        </form>
        {note && <output className="mb-2 block text-xs opacity-80">{note}</output>}
        {all.length === 0 ? (
          <p className="opacity-70">
            None saved yet. A workspace keeps every open tab, its splits, and the host or shell in
            each pane, to open again later.
          </p>
        ) : (
          <ul aria-label="Saved workspaces" className="flex flex-col gap-1">
            {all.map((workspace) => {
              const panes = workspace.tabs.reduce((sum, t) => sum + templatePanes(t), 0);
              const direct = directConnections(workspace);
              return (
                <li key={workspace.id} className="flex flex-wrap items-center gap-2">
                  <span className="min-w-0 flex-1 truncate">{workspace.name}</span>
                  <span className="text-xs opacity-60">
                    {workspace.tabs.length} tabs, {panes} panes
                  </span>
                  <button
                    type="button"
                    className={BUTTON}
                    aria-label={`Open ${workspace.name}`}
                    onClick={() => onOpen(workspace.id)}
                  >
                    Open
                  </button>
                  <button
                    type="button"
                    className={BUTTON}
                    aria-label={`Delete ${workspace.name}`}
                    onClick={() => workspaces.remove(workspace.id)}
                  >
                    Delete
                  </button>
                  {direct.length > 0 && (
                    <span className="w-full truncate text-xs opacity-60" title={direct.join("\n")}>
                      Connects to {direct.join(", ")}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </dialog>
    </div>
  );
}
