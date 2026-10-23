// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Files in memory for end-to-end tests. Only the `e2e` build loads it. Each
// side is a tree of folders and files; a transfer copies a file between
// them over a few ticks, and a drop can be simulated through
// `window.tauminusTestDrop(paths)`.

import type {
  ChangeSpec,
  DroppedFiles,
  EditView,
  EntryView,
  FilesClient,
  Listing,
  TransferView,
} from "./files.ts";

interface Node {
  dir: boolean;
  size: number;
  mode: number;
}

type Tree = Map<string, Node>;

function tree(entries: [string, Node][]): Tree {
  return new Map(entries);
}

const FILE = (size: number): Node => ({ dir: false, size, mode: 0o644 });
const DIR: Node = { dir: true, size: 0, mode: 0o755 };

function parentOf(path: string): string | null {
  if (path === "/") return null;
  const at = path.lastIndexOf("/");
  return at <= 0 ? "/" : path.slice(0, at);
}

function join(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

function nameOf(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

function listing(nodes: Tree, path: string): Listing {
  const node = nodes.get(path);
  if (!node?.dir) throw new Error(`no folder ${path}`);
  const entries: EntryView[] = [];
  for (const [p, n] of nodes) {
    if (p !== path && parentOf(p) === path) {
      entries.push({
        name: nameOf(p),
        kind: n.dir ? "dir" : "file",
        size: n.size,
        modified: 0,
        mode: n.mode,
      });
    }
  }
  return { path, parent: parentOf(path), entries };
}

declare global {
  interface Window {
    tauminusTestDrop?: (paths: string[]) => void;
  }
}

export function fakeFiles(): FilesClient {
  const local = tree([
    ["/", DIR],
    ["/home", DIR],
    ["/home/me", DIR],
    ["/home/me/notes.txt", FILE(1200)],
    ["/home/me/photos", DIR],
    ["/home/me/photos/cat.jpg", FILE(48_000)],
  ]);
  const remote = tree([
    ["/", DIR],
    ["/home", DIR],
    ["/home/ops", DIR],
    ["/home/ops/app.log", FILE(5_000_000)],
    ["/home/ops/deploy", DIR],
    ["/home/ops/deploy/run.sh", FILE(300)],
  ]);
  const transfers = new Map<number, TransferView>();
  const edits = new Map<number, EditView>();
  let next = 1;
  const sessions = new Set<number>();
  let drop: ((files: DroppedFiles) => void) | undefined;
  if (typeof window !== "undefined") {
    window.tauminusTestDrop = (paths) => drop?.({ paths, x: 0, y: 0 });
  }

  const start = (
    id: number,
    direction: "download" | "upload",
    from: Tree,
    path: string,
    to: Tree,
    into: string,
    overwrite: boolean,
  ): number => {
    if (!sessions.has(id)) throw new Error("no such file session");
    const source = from.get(path);
    if (!source || source.dir) throw new Error(`${path} is not a file`);
    const name = nameOf(path);
    const target = join(into, name);
    if (!overwrite && to.has(target)) throw new Error(`${target} already exists`);
    const transfer = next++;
    const view: TransferView = {
      id: transfer,
      session: id,
      direction,
      name,
      done: 0,
      total: source.size,
      resumedFrom: 0,
      status: { kind: "running" },
    };
    transfers.set(transfer, view);
    let step = 0;
    const tick = setInterval(() => {
      const current = transfers.get(transfer);
      if (current?.status.kind !== "running") {
        clearInterval(tick);
        return;
      }
      step += 1;
      const done = Math.min(source.size, Math.ceil((source.size * step) / 4));
      const finished = done === source.size;
      transfers.set(transfer, {
        ...current,
        done,
        status: finished ? { kind: "done" } : current.status,
      });
      if (finished) {
        to.set(target, { ...source });
        clearInterval(tick);
      }
    }, 100);
    return transfer;
  };

  const change = (c: ChangeSpec) => {
    switch (c.kind) {
      case "mkdir":
        if (remote.has(c.path)) throw new Error(`${c.path} exists`);
        remote.set(c.path, DIR);
        return;
      case "removeFile":
      case "removeDir":
        if (!remote.delete(c.path)) throw new Error(`no ${c.path}`);
        return;
      case "rename": {
        const node = remote.get(c.from);
        if (!node) throw new Error(`no ${c.from}`);
        remote.delete(c.from);
        remote.set(c.to, node);
        return;
      }
      case "chmod": {
        const node = remote.get(c.path);
        if (!node) throw new Error(`no ${c.path}`);
        remote.set(c.path, { ...node, mode: c.mode });
        return;
      }
    }
  };

  return {
    open: (host) => {
      if (host === "") return Promise.reject(new Error("no host"));
      const id = next++;
      sessions.add(id);
      return Promise.resolve({ id, home: "/home/ops" });
    },
    close: (id) => {
      sessions.delete(id);
      return Promise.resolve();
    },
    list: (_id, path) => {
      try {
        return Promise.resolve(listing(remote, path === "." ? "/home/ops" : path));
      } catch (error) {
        return Promise.reject(error instanceof Error ? error : new Error(String(error)));
      }
    },
    change: (_id, c) => {
      try {
        change(c);
        return Promise.resolve();
      } catch (error) {
        return Promise.reject(error instanceof Error ? error : new Error(String(error)));
      }
    },
    localList: (path) => {
      try {
        return Promise.resolve(listing(local, path));
      } catch (error) {
        return Promise.reject(error instanceof Error ? error : new Error(String(error)));
      }
    },
    localHome: () => Promise.resolve("/home/me"),
    download: (id, path, into, overwrite) =>
      Promise.resolve(start(id, "download", remote, path, local, into, overwrite)),
    upload: (id, path, into, overwrite) =>
      Promise.resolve(start(id, "upload", local, path, remote, into, overwrite)),
    transfers: () => Promise.resolve([...transfers.values()]),
    cancel: (transfer) => {
      const view = transfers.get(transfer);
      if (view?.status.kind !== "running") return Promise.resolve(false);
      transfers.set(transfer, { ...view, status: { kind: "cancelled" } });
      return Promise.resolve(true);
    },
    edit: (id, path) => {
      const node = remote.get(path);
      if (!node || node.dir) return Promise.reject(new Error("only a file can be edited"));
      const edit = next++;
      edits.set(edit, {
        id: edit,
        session: id,
        remote: path,
        local: `/tmp/edit/${nameOf(path)}`,
        state: { kind: "watching" },
      });
      return Promise.resolve(edit);
    },
    editDone: (edit) => Promise.resolve(edits.delete(edit)),
    edits: () => Promise.resolve([...edits.values()]),
    onDrop: (onDrop) => {
      drop = onDrop;
      return Promise.resolve();
    },
  };
}
