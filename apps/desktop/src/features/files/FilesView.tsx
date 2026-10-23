// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FormEvent,
  memo,
  useCallback,
  useEffect,
  useId,
  useState,
  useSyncExternalStore,
} from "react";

import { PromptDialog } from "@/features/ssh/PromptDialog.tsx";
import type { EditView, EntryView, FilesClient, TransferView } from "@/ipc/index.ts";

import { type FilesStore, type Side, joinPath, modeText, parseMode } from "./files-store.ts";

const BUTTON = "btn btn-sm text-xs";
const FIELD = "field min-w-0 flex-1 font-mono text-xs";

/** `1.2 MB`, in powers of 1000 as file managers show them. */
export function sizeText(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = "B";
  for (const next of units) {
    if (value < 1000) break;
    value /= 1000;
    unit = next;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${unit}`;
}

/**
 * Files on a host beside files here, with the transfers between them.
 * Files dropped on the window from the OS go to the remote folder.
 */
export function FilesView({
  store,
  client,
  hostLabel,
  onClose,
}: {
  store: FilesStore;
  client: FilesClient;
  hostLabel: string;
  onClose: () => void;
}) {
  const titleId = useId();
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  useEffect(() => {
    void client.onDrop((files) => void store.uploadPaths(files.paths));
    return () => void client.onDrop(() => {});
  }, [client, store]);
  // On the window: focus may sit outside the view, as after a button it
  // held went away.
  const prompting = state.prompt !== undefined;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !prompting && !event.defaultPrevented) {
        event.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, prompting]);

  return (
    <dialog
      open
      aria-labelledby={titleId}
      className="fixed inset-0 z-30 m-0 flex h-full max-h-none w-full max-w-none flex-col bg-background p-0 text-sm text-foreground"
    >
      <header className="flex items-center gap-3 border-b border-current/10 px-4 py-2">
        <h2 id={titleId} className="font-medium">
          Files on {hostLabel}
        </h2>
        <label className="ml-auto flex items-center gap-1 text-xs">
          <input
            type="checkbox"
            checked={state.overwrite}
            onChange={(e) => store.setOverwrite(e.target.checked)}
          />
          Replace files of the same name
        </label>
        <button type="button" className={BUTTON} onClick={() => void store.refresh()}>
          Refresh
        </button>
        <button type="button" className={BUTTON} onClick={onClose}>
          Close
        </button>
      </header>
      {state.status === "opening" && <p className="p-4 opacity-70">Connecting…</p>}
      {state.status === "failed" && (
        <div className="p-4">
          <p role="alert" className="mb-2 text-error">
            {state.error}
          </p>
          <button type="button" className={BUTTON} onClick={() => void store.open()}>
            Try again
          </button>
        </div>
      )}
      {state.status === "open" && (
        <>
          {state.notice && (
            <div role="alert" className="flex items-center gap-2 bg-red-500/10 px-4 py-1 text-xs">
              <span className="min-w-0 flex-1">{state.notice}</span>
              <button type="button" className={BUTTON} onClick={() => store.dismissNotice()}>
                Dismiss
              </button>
            </div>
          )}
          <div className="grid min-h-0 flex-1 grid-cols-2 grid-rows-[minmax(0,1fr)] divide-x divide-current/10">
            <Pane title="This computer" which="local" side={state.local} store={store} />
            <Pane title={hostLabel} which="remote" side={state.remote} store={store} />
          </div>
          <Edits edits={state.edits} onDone={(id) => void store.editDone(id)} />
          <Transfers transfers={state.transfers} store={store} />
        </>
      )}
      {state.prompt && <PromptDialog prompt={state.prompt.prompt} onAnswer={state.prompt.answer} />}
    </dialog>
  );
}

/** Height of one row, fixed so the list can show only what is in view. */
const ROW = 28;
/** Rows drawn above and below what is in view. */
const OVERSCAN = 10;

/**
 * One side's folder. It renders again only when that side changes, not
 * on a transfer tick, and draws only the rows in view: a folder of 100 000
 * entries costs what one screenful does.
 */
const Pane = memo(function Pane({
  title,
  which,
  side,
  store,
}: {
  title: string;
  which: "local" | "remote";
  side: Side;
  store: FilesStore;
}) {
  const [typed, setTyped] = useState<string | undefined>();
  // The scroll offset belongs to the folder it was read in.
  const [view, setView] = useState({ top: 0, height: 800, path: side.path });
  const measure = useCallback((element: HTMLDivElement | null) => {
    if (!element) return undefined;
    const observer = new ResizeObserver(() =>
      setView((v) => ({ ...v, height: element.clientHeight })),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const go = useCallback(
    (path: string) => void (which === "local" ? store.goLocal(path) : store.goRemote(path)),
    [store, which],
  );
  const select = useCallback(
    (name: string, toggle: boolean) => store.select(which, name, toggle),
    [store, which],
  );
  const path = side.path;
  const open = useCallback(
    (entry: EntryView) => {
      if (entry.kind === "dir") go(joinPath(path, entry.name));
      else {
        // Opening a row acts on that row, whatever was selected before.
        store.select(which, entry.name, false);
        void (which === "local" ? store.upload() : store.download());
      }
    },
    [go, path, store, which],
  );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (typed?.trim()) go(typed.trim());
    setTyped(undefined);
  };
  const count = side.entries.length;
  const top = view.path === path ? view.top : 0;
  const first = Math.max(0, Math.floor(top / ROW) - OVERSCAN);
  const last = Math.min(count, Math.ceil((top + view.height) / ROW) + OVERSCAN);
  return (
    <section className="flex min-h-0 flex-col" aria-label={title}>
      <div className="flex items-center gap-2 border-b border-current/10 px-3 py-2">
        <span className="shrink-0 font-medium">{title}</span>
        <button
          type="button"
          className={BUTTON}
          aria-label={`Up from ${side.path}`}
          disabled={side.parent === null}
          onClick={() => side.parent !== null && go(side.parent)}
        >
          ↑
        </button>
        <form className="flex min-w-0 flex-1" onSubmit={submit}>
          <input
            aria-label={`${title} folder`}
            value={typed ?? side.path}
            onChange={(e) => setTyped(e.target.value)}
            spellCheck={false}
            className={FIELD}
          />
        </form>
        {which === "local" ? (
          <button
            type="button"
            className={BUTTON}
            disabled={side.selected.size === 0}
            onClick={() => void store.upload()}
          >
            Upload →
          </button>
        ) : (
          <RemoteActions store={store} side={side} />
        )}
      </div>
      {side.error && (
        <p role="alert" className="px-3 py-1 text-xs text-error">
          {side.error}
        </p>
      )}
      <div
        // A new folder gets a new scroller, starting at its top.
        key={path}
        ref={measure}
        className="min-h-0 flex-1 overflow-y-auto"
        onScroll={(e) => {
          const at = e.currentTarget.scrollTop;
          setView((v) => (v.top === at && v.path === path ? v : { ...v, top: at, path }));
        }}
      >
        <ul aria-label={`${title} entries`} style={{ height: count * ROW, position: "relative" }}>
          {side.entries.slice(first, last).map((entry, i) => (
            <EntryRow
              key={entry.name}
              entry={entry}
              top={(first + i) * ROW}
              selected={side.selected.has(entry.name)}
              showMode={which === "remote"}
              onSelect={select}
              onOpen={open}
            />
          ))}
        </ul>
        {!side.loading && count === 0 && !side.error && (
          <p className="px-3 py-2 opacity-60">Empty folder</p>
        )}
      </div>
    </section>
  );
});

const EntryRow = memo(function EntryRow({
  entry,
  top,
  selected,
  showMode,
  onSelect,
  onOpen,
}: {
  entry: EntryView;
  top: number;
  selected: boolean;
  showMode: boolean;
  onSelect: (name: string, toggle: boolean) => void;
  onOpen: (entry: EntryView) => void;
}) {
  const glyph = entry.kind === "dir" ? "▸" : entry.kind === "link" ? "↪" : " ";
  return (
    <li className="absolute right-0 left-0" style={{ top, height: ROW }}>
      <button
        type="button"
        aria-pressed={selected}
        onClick={(e) => onSelect(entry.name, e.metaKey || e.ctrlKey)}
        onDoubleClick={() => onOpen(entry)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            onOpen(entry);
          }
        }}
        className={`grid h-7 w-full grid-cols-[1.25rem_1fr_6rem_6rem] items-center gap-2 px-3 text-left ${selected ? "bg-current/15" : "hover:bg-current/5"}`}
      >
        <span aria-hidden className="opacity-60">
          {glyph}
        </span>
        <span className="truncate">{entry.name}</span>
        <span className="text-right text-xs opacity-70">
          {entry.kind === "file" ? sizeText(entry.size ?? 0) : ""}
        </span>
        <span className="font-mono text-xs opacity-60">{showMode ? modeText(entry.mode) : ""}</span>
      </button>
    </li>
  );
});

type Editing =
  | { kind: "mkdir" }
  | { kind: "rename"; name: string }
  | { kind: "chmod"; name: string };

function RemoteActions({ store, side }: { store: FilesStore; side: Side }) {
  const [editing, setEditing] = useState<Editing | undefined>();
  const [text, setText] = useState("");
  const [problem, setProblem] = useState<string | undefined>();
  const only = side.selected.size === 1 ? [...side.selected][0] : undefined;
  const entry = side.entries.find((e) => e.name === only);
  const begin = (next: Editing, initial: string) => {
    setEditing(next);
    setText(initial);
    setProblem(undefined);
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!editing) return;
    const name = text.trim();
    if (editing.kind === "chmod") {
      const mode = parseMode(name);
      if (mode === undefined) {
        setProblem("Write the mode in octal, as 644 or 0755");
        return;
      }
      if (await store.change({ kind: "chmod", path: joinPath(side.path, editing.name), mode })) {
        setEditing(undefined);
      }
      return;
    }
    if (!name || name.includes("/")) {
      setProblem("A name, without /");
      return;
    }
    const done =
      editing.kind === "mkdir"
        ? await store.change({ kind: "mkdir", path: joinPath(side.path, name) })
        : await store.change({
            kind: "rename",
            from: joinPath(side.path, editing.name),
            to: joinPath(side.path, name),
          });
    if (done) setEditing(undefined);
  };
  if (editing) {
    const label =
      editing.kind === "mkdir"
        ? "New folder name"
        : editing.kind === "rename"
          ? `New name for ${editing.name}`
          : `Permissions of ${editing.name}`;
    return (
      <form className="flex items-center gap-1" onSubmit={(e) => void submit(e)}>
        <input
          aria-label={label}
          placeholder={label}
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
          // oxlint-disable-next-line jsx-a11y/no-autofocus -- the field the user just asked for
          autoFocus
          className={`${FIELD} w-40`}
        />
        <button type="submit" className={BUTTON}>
          OK
        </button>
        <button type="button" className={BUTTON} onClick={() => setEditing(undefined)}>
          Cancel
        </button>
        {problem && <span className="text-xs text-error">{problem}</span>}
      </form>
    );
  }
  return (
    <div className="flex shrink-0 gap-1">
      <button
        type="button"
        className={BUTTON}
        disabled={side.selected.size === 0}
        onClick={() => void store.download()}
      >
        ← Download
      </button>
      <button type="button" className={BUTTON} onClick={() => begin({ kind: "mkdir" }, "")}>
        New folder
      </button>
      <button
        type="button"
        className={BUTTON}
        disabled={entry?.kind !== "file"}
        onClick={() => void store.edit()}
      >
        Edit
      </button>
      <button
        type="button"
        className={BUTTON}
        disabled={!only}
        onClick={() => only && begin({ kind: "rename", name: only }, only)}
      >
        Rename
      </button>
      <button
        type="button"
        className={BUTTON}
        disabled={!entry}
        onClick={() =>
          entry && begin({ kind: "chmod", name: entry.name }, (entry.mode ?? 0o644).toString(8))
        }
      >
        Permissions
      </button>
      <button
        type="button"
        className={BUTTON}
        disabled={!entry}
        onClick={() =>
          entry &&
          void store.change({
            kind: entry.kind === "dir" ? "removeDir" : "removeFile",
            path: joinPath(side.path, entry.name),
          })
        }
      >
        Delete
      </button>
    </div>
  );
}

function Transfers({
  transfers,
  store,
}: {
  transfers: readonly TransferView[];
  store: FilesStore;
}) {
  const cancel = useCallback((id: number) => void store.cancel(id), [store]);
  if (transfers.length === 0) return null;
  // Each tick makes new objects for every transfer; rows get plain values
  // so only those that moved draw again.
  return (
    <ul
      aria-label="Transfers"
      className="max-h-40 overflow-y-auto border-t border-current/10 px-4 py-2"
    >
      {transfers.map((t) => (
        <TransferRow
          key={t.id}
          id={t.id}
          name={t.name}
          download={t.direction === "download"}
          done={t.done ?? 0}
          total={t.total ?? 0}
          resumed={t.resumedFrom ?? 0}
          kind={t.status.kind}
          reason={t.status.kind === "failed" ? t.status.reason : ""}
          onCancel={cancel}
        />
      ))}
    </ul>
  );
}

const TransferRow = memo(function TransferRow({
  id,
  name,
  download,
  done,
  total,
  resumed,
  kind,
  reason,
  onCancel,
}: {
  id: number;
  name: string;
  download: boolean;
  done: number;
  total: number;
  resumed: number;
  kind: TransferView["status"]["kind"];
  reason: string;
  onCancel: (id: number) => void;
}) {
  const share = total > 0 ? Math.round((done / total) * 100) : 100;
  const status =
    kind === "failed"
      ? reason
      : kind === "running"
        ? `${share}%${resumed > 0 ? `, resumed at ${sizeText(resumed)}` : ""}`
        : kind;
  return (
    <li className="flex items-center gap-3 py-0.5 text-xs">
      <span className="w-4 opacity-60">{download ? "↓" : "↑"}</span>
      <span className="w-48 truncate">{name}</span>
      <progress max={100} value={share} className="h-1.5 w-40" aria-label={`${name} progress`} />
      <span className="min-w-0 flex-1 truncate opacity-80">{status}</span>
      {(kind === "running" || kind === "waiting") && (
        <button type="button" className={BUTTON} onClick={() => onCancel(id)}>
          Cancel
        </button>
      )}
    </li>
  );
});

const EDIT_STATES: Record<EditView["state"]["kind"], string> = {
  watching: "open in the editor; each save goes to the server",
  saved: "saved to the server",
  uploading: "saving to the server…",
  conflict: "changed on the server since it opened; not saved over it. Your copy:",
  failed: "",
};

function Edits({ edits, onDone }: { edits: readonly EditView[]; onDone: (id: number) => void }) {
  if (edits.length === 0) return null;
  return (
    <ul aria-label="Editing" className="border-t border-current/10 px-4 py-2">
      {edits.map((e) => (
        <li key={e.id} className="flex items-center gap-3 py-0.5 text-xs">
          <span className="w-64 truncate font-mono">{e.remote}</span>
          <span className="min-w-0 flex-1 truncate opacity-80">
            {e.state.kind === "failed" ? e.state.reason : EDIT_STATES[e.state.kind]}
            {e.state.kind === "failed" && " Your copy:"}
            {(e.state.kind === "conflict" || e.state.kind === "failed") && (
              <span className="ml-1 font-mono">{e.local}</span>
            )}
          </span>
          <button type="button" className={BUTTON} onClick={() => onDone(e.id)}>
            Done
          </button>
        </li>
      ))}
    </ul>
  );
}
