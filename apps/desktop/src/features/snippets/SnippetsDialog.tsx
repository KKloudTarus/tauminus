// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FormEvent,
  memo,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

import type { SnippetView } from "@/ipc/index.ts";

import { type SnippetsStore, byFolder } from "./snippets-store.ts";

const BUTTON = "rounded px-2 py-0.5 hover:bg-current/10 disabled:opacity-40";
const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1 outline-none focus:border-current/50";

/** What the form edits: a new snippet, or a saved one. */
type Editing = { kind: "new" } | { kind: "edit"; snippet: SnippetView };

/** The snippet library: run, add, edit, and delete, by folder. */
export function SnippetsDialog({
  store,
  onRun,
  onRunOnHosts,
  onClose,
}: {
  store: SnippetsStore;
  onRun: (id: string) => void;
  /** Asks for saved hosts to run it on; without it there is no such button. */
  onRunOnHosts?: (id: string) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dialog = useRef<HTMLDialogElement>(null);
  const [editing, setEditing] = useState<Editing | undefined>();
  useEffect(() => dialog.current?.focus(), []);
  const folders = byFolder(state.snippets);
  const edit = useCallback(
    (id: string) => {
      const snippet = store.find(id);
      if (snippet) setEditing({ kind: "edit", snippet });
    },
    [store],
  );
  const remove = useCallback((id: string) => void store.remove(id), [store]);
  const folderNames = folders.flatMap((f) => (f.name ? [f.name] : []));

  return (
    <div className="fixed inset-0 z-20 flex items-start justify-center bg-black/30 pt-[10vh]">
      <dialog
        ref={dialog}
        open
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            if (editing) setEditing(undefined);
            else onClose();
          }
        }}
        className="static m-0 max-h-[75vh] w-[min(640px,92vw)] overflow-y-auto rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl outline-none"
      >
        <div className="mb-3 flex items-center justify-between gap-2">
          <h2 id={titleId} className="text-base font-medium">
            Snippets
          </h2>
          <div className="flex gap-1">
            <button type="button" className={BUTTON} onClick={() => setEditing({ kind: "new" })}>
              New snippet
            </button>
            <button type="button" className={BUTTON} onClick={onClose}>
              Close
            </button>
          </div>
        </div>
        {state.error && (
          <p role="alert" className="mb-2 text-red-600 dark:text-red-400">
            {state.error}
          </p>
        )}
        {editing && (
          <SnippetEditor
            key={editing.kind === "edit" ? editing.snippet.id : "new"}
            snippet={editing.kind === "edit" ? editing.snippet : undefined}
            folders={folderNames}
            onSave={async (input) => {
              if (await store.save(input)) setEditing(undefined);
            }}
            onCancel={() => setEditing(undefined)}
          />
        )}
        {state.loaded && state.snippets.length === 0 && !editing && (
          <p className="opacity-70">
            No snippets yet. A snippet is a command you save once and run in any terminal;{" "}
            <code>{"{{name}}"}</code> asks for a value, <code>{"{{name:default}}"}</code> offers
            one.
          </p>
        )}
        {folders.map((folder) => (
          <section key={folder.name ?? ""} aria-label={folder.name ?? "No folder"} className="mb-3">
            {folder.name && <h3 className="mb-1 font-medium">{folder.name}</h3>}
            <ul className="flex flex-col gap-1">
              {folder.snippets.map((snippet) => (
                <SnippetRow
                  key={snippet.id}
                  id={snippet.id}
                  name={snippet.name}
                  preview={preview(snippet.body)}
                  onRun={onRun}
                  onRunOnHosts={onRunOnHosts}
                  onEdit={edit}
                  onDelete={remove}
                />
              ))}
            </ul>
          </section>
        ))}
      </dialog>
    </div>
  );
}

/** The first line of a body, short: a 64 KiB body would cost its layout in every row. */
function preview(body: string): string {
  const first = body.trimStart().split("\n", 1)[0] ?? "";
  const more = first.length < body.trim().length;
  return first.length > 200 ? `${first.slice(0, 200)}…` : more ? `${first} …` : first;
}

/** Plain values only, so a list returned anew draws only the rows that changed. */
const SnippetRow = memo(function SnippetRow({
  id,
  name,
  preview: text,
  onRun,
  onRunOnHosts,
  onEdit,
  onDelete,
}: {
  id: string;
  name: string;
  preview: string;
  onRun: (id: string) => void;
  onRunOnHosts: ((id: string) => void) | undefined;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
}) {
  return (
    <li className="flex items-center gap-2">
      <span className="w-40 shrink-0 truncate">{name}</span>
      <code className="min-w-0 flex-1 truncate text-xs opacity-70">{text}</code>
      <button type="button" className={BUTTON} aria-label={`Run ${name}`} onClick={() => onRun(id)}>
        Run
      </button>
      {onRunOnHosts && (
        <button
          type="button"
          className={BUTTON}
          aria-label={`Run ${name} on hosts`}
          onClick={() => onRunOnHosts(id)}
        >
          On hosts…
        </button>
      )}
      <button
        type="button"
        className={BUTTON}
        aria-label={`Edit ${name}`}
        onClick={() => onEdit(id)}
      >
        Edit
      </button>
      <button
        type="button"
        className={BUTTON}
        aria-label={`Delete ${name}`}
        onClick={() => onDelete(id)}
      >
        Delete
      </button>
    </li>
  );
});

function SnippetEditor({
  snippet,
  folders,
  onSave,
  onCancel,
}: {
  snippet: SnippetView | undefined;
  folders: readonly string[];
  onSave: (input: {
    id: string | null;
    name: string;
    folder: string | null;
    body: string;
  }) => Promise<void>;
  onCancel: () => void;
}) {
  const listId = useId();
  const [name, setName] = useState(snippet?.name ?? "");
  const [folder, setFolder] = useState(snippet?.folder ?? "");
  const [body, setBody] = useState(snippet?.body ?? "");
  const submit = (event: FormEvent) => {
    event.preventDefault();
    void onSave({ id: snippet?.id ?? null, name, folder: folder.trim() || null, body });
  };
  return (
    <form
      aria-label={snippet ? `Edit ${snippet.name}` : "New snippet"}
      onSubmit={submit}
      className="mb-4 flex flex-col gap-2 rounded border border-current/15 p-3"
    >
      <div className="flex gap-2">
        <label className="flex flex-1 flex-col gap-1">
          Name
          <input
            className={FIELD}
            value={name}
            required
            maxLength={128}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <label className="flex flex-1 flex-col gap-1">
          Folder
          <input
            className={FIELD}
            value={folder}
            list={listId}
            maxLength={128}
            placeholder="None"
            onChange={(e) => setFolder(e.target.value)}
          />
          <datalist id={listId}>
            {folders.map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </datalist>
        </label>
      </div>
      <label className="flex flex-col gap-1">
        Command
        <textarea
          className={`${FIELD} font-mono`}
          value={body}
          required
          rows={4}
          spellCheck={false}
          placeholder="tail -n {{lines:100}} {{file}}"
          onChange={(e) => setBody(e.target.value)}
        />
      </label>
      <div className="flex justify-end gap-2">
        <button type="button" className={BUTTON} onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className={BUTTON}>
          Save
        </button>
      </div>
    </form>
  );
}
