// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { PendingPrompt } from "@/features/forwards/forwards-store.ts";
import { messageOf } from "@/features/hosts/hosts-store.ts";
import {
  type AnswerSpec,
  type ChangeSpec,
  type EditView,
  type EntryView,
  type FilesClient,
  type PromptKind,
  SessionCommandError,
  type TransferView,
} from "@/ipc/index.ts";

/** One side of the view: a folder here or on the server. */
export interface Side {
  readonly path: string;
  readonly parent: string | null;
  readonly entries: readonly EntryView[];
  readonly loading: boolean;
  readonly error: string | undefined;
  /** Names of the selected entries. */
  readonly selected: ReadonlySet<string>;
}

export interface FilesState {
  readonly status: "opening" | "open" | "failed" | "closed";
  readonly error: string | undefined;
  readonly local: Side;
  readonly remote: Side;
  /** This session's transfers, oldest first. */
  readonly transfers: readonly TransferView[];
  /** Files open in a local editor. */
  readonly edits: readonly EditView[];
  /** Replace a file of the same name at the destination. */
  readonly overwrite: boolean;
  /** Why files could not be queued, until the next copy or dismissal. */
  readonly notice: string | undefined;
  readonly prompt: PendingPrompt | undefined;
}

const EMPTY_SIDE: Side = {
  path: "",
  parent: null,
  entries: [],
  loading: false,
  error: undefined,
  selected: new Set(),
};

const CANCEL: AnswerSpec = { kind: "cancel" };

/** Folders first, then by name, as file managers list them. O(n log n). */
export function sorted(entries: readonly EntryView[]): EntryView[] {
  return entries.toSorted((a, b) => {
    const dirs = Number(b.kind === "dir") - Number(a.kind === "dir");
    return dirs !== 0 ? dirs : a.name.localeCompare(b.name);
  });
}

/** `dir` and `name` as one path, with the separator the side uses. */
export function joinPath(dir: string, name: string): string {
  const separator = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  return dir.endsWith(separator) ? `${dir}${name}` : `${dir}${separator}${name}`;
}

/** A mode as `chmod` writes it, `0644`; `undefined` when it is not one. */
export function parseMode(text: string): number | undefined {
  const trimmed = text.trim();
  if (!/^0?[0-7]{3,4}$/.test(trimmed)) return undefined;
  return Number.parseInt(trimmed, 8);
}

/** `rwxr-xr-x` for 0o755. */
export function modeText(mode: number | null | undefined): string {
  if (mode == null) return "";
  const bits = "rwxrwxrwx";
  let text = "";
  for (let i = 0; i < 9; i += 1) text += mode & (1 << (8 - i)) ? bits[i] : "-";
  return text;
}

/**
 * One file session with a host, outside React: both folders, what is
 * selected in each, and this session's transfers, read once a second
 * while any run. A finished transfer refreshes the side it landed on.
 */
export class FilesStore {
  readonly #client: FilesClient;
  readonly #host: string;
  readonly #unlock: (retry: () => void) => void;
  readonly #listeners = new Set<() => void>();
  #session: number | undefined;
  #timer: ReturnType<typeof setInterval> | undefined;
  /** A status read is under way; a tick meanwhile is skipped. */
  #reading = false;
  /** A read was asked for while one ran; it runs again after. */
  #readAgain = false;
  /** Counts calls to watch, so a read that began before one never stops the timer. */
  #watched = 0;
  /** When each side was last listed again because a transfer landed. */
  readonly #landed = { local: 0, remote: 0 };
  readonly #pendingReload = { local: false, remote: false };
  #state: FilesState = {
    status: "opening",
    error: undefined,
    local: EMPTY_SIDE,
    remote: EMPTY_SIDE,
    transfers: [],
    edits: [],
    overwrite: false,
    notice: undefined,
    prompt: undefined,
  };

  constructor(client: FilesClient, host: string, unlock: (retry: () => void) => void) {
    this.#client = client;
    this.#host = host;
    this.#unlock = unlock;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): FilesState => this.#state;

  /** Signs in (asking what it must) and shows both home folders. */
  async open(): Promise<void> {
    this.#set({ ...this.#state, status: "opening", error: undefined });
    try {
      const [handle, home] = await Promise.all([
        this.#client.open(this.#host, (prompt, withdrawn) => this.#ask(prompt, withdrawn)),
        this.#client.localHome(),
      ]);
      if (this.#state.status === "closed") {
        await this.#client.close(handle.id);
        return;
      }
      this.#session = handle.id;
      this.#set({ ...this.#state, status: "open" });
      await Promise.all([this.goLocal(home), this.goRemote(handle.home)]);
    } catch (error) {
      if (error instanceof SessionCommandError && error.code === "vault_locked") {
        this.#unlock(() => void this.open());
      }
      this.#set({ ...this.#state, status: "failed", error: messageOf(error) });
    } finally {
      this.#state.prompt?.answer(CANCEL);
    }
  }

  close(): void {
    this.#stopWatching();
    // Open edits end with the view; each sends its last save first.
    for (const edit of this.#state.edits) void this.#client.editDone(edit.id);
    if (this.#session !== undefined) void this.#client.close(this.#session);
    this.#session = undefined;
    this.#set({ ...this.#state, status: "closed" });
  }

  async goLocal(path: string): Promise<void> {
    await this.#load("local", path, (p) => this.#client.localList(p));
  }

  async goRemote(path: string): Promise<void> {
    const session = this.#session;
    if (session === undefined) return;
    await this.#load("remote", path, (p) => this.#client.list(session, p));
  }

  refresh(): Promise<void> {
    return Promise.all([
      this.goLocal(this.#state.local.path),
      this.goRemote(this.#state.remote.path),
    ]).then(() => {});
  }

  /** Click selects one entry; with `toggle`, adds or removes it. */
  select(side: "local" | "remote", name: string, toggle = false): void {
    const current = this.#state[side];
    const selected = new Set(toggle ? current.selected : []);
    if (toggle && selected.has(name)) selected.delete(name);
    else selected.add(name);
    this.#set({ ...this.#state, [side]: { ...current, selected } });
  }

  dismissNotice(): void {
    this.#set({ ...this.#state, notice: undefined });
  }

  setOverwrite(overwrite: boolean): void {
    this.#set({ ...this.#state, overwrite });
  }

  /** Copies the selected remote files into the local folder. */
  download(): Promise<void> {
    const { remote, local, overwrite } = this.#state;
    return this.#queue("remote", (id, name) =>
      this.#client.download(id, joinPath(remote.path, name), local.path, overwrite),
    );
  }

  /** Copies the selected local files into the remote folder. */
  upload(): Promise<void> {
    const { remote, local, overwrite } = this.#state;
    return this.#queue("local", (id, name) =>
      this.#client.upload(id, joinPath(local.path, name), remote.path, overwrite),
    );
  }

  /** Copies files dropped from the OS into the remote folder. */
  async uploadPaths(paths: readonly string[]): Promise<void> {
    const session = this.#session;
    if (session === undefined) return;
    const { remote, overwrite } = this.#state;
    const failures: string[] = [];
    for (const path of paths) {
      try {
        // oxlint-disable-next-line no-await-in-loop -- queued in order, each answer before the next
        await this.#client.upload(session, path, remote.path, overwrite);
      } catch (error) {
        failures.push(messageOf(error));
      }
    }
    await this.#afterQueue(failures);
  }

  /** One change on the server, then the remote folder again. */
  async change(change: ChangeSpec): Promise<boolean> {
    const session = this.#session;
    if (session === undefined) return false;
    try {
      await this.#client.change(session, change);
      await this.goRemote(this.#state.remote.path);
      return true;
    } catch (error) {
      this.#setSide("remote", { error: messageOf(error) });
      return false;
    }
  }

  /** Opens the selected remote file in a local editor. */
  async edit(): Promise<void> {
    const session = this.#session;
    const { remote } = this.#state;
    const name = [...remote.selected][0];
    if (session === undefined || name === undefined) return;
    try {
      await this.#client.edit(session, joinPath(remote.path, name));
      this.#set({ ...this.#state, notice: undefined });
    } catch (error) {
      this.#set({ ...this.#state, notice: `${name}: ${messageOf(error)}` });
    }
    this.#watch();
    await this.#readTransfers();
  }

  async editDone(edit: number): Promise<void> {
    await this.#client.editDone(edit);
    await this.#readTransfers();
  }

  cancel(transfer: number): Promise<boolean> {
    return this.#client.cancel(transfer);
  }

  async #queue(
    from: "local" | "remote",
    start: (session: number, name: string) => Promise<number>,
  ): Promise<void> {
    const session = this.#session;
    if (session === undefined) return;
    const side = this.#state[from];
    const files = side.entries.filter((e) => side.selected.has(e.name) && e.kind === "file");
    const failures: string[] = [];
    for (const file of files) {
      try {
        // oxlint-disable-next-line no-await-in-loop -- queued in order, each answer before the next
        await start(session, file.name);
      } catch (error) {
        failures.push(`${file.name}: ${messageOf(error)}`);
      }
    }
    await this.#afterQueue(failures);
  }

  async #afterQueue(failures: string[]): Promise<void> {
    this.#set({ ...this.#state, notice: failures.length > 0 ? failures.join("; ") : undefined });
    this.#watch();
    await this.#readTransfers();
  }

  async #load(
    side: "local" | "remote",
    path: string,
    list: (path: string) => Promise<{
      path: string;
      parent: string | null;
      entries: EntryView[];
    }>,
  ): Promise<void> {
    this.#setSide(side, { loading: true, error: undefined });
    try {
      const listing = await list(path);
      const keep = this.#state[side].path === listing.path;
      this.#set({
        ...this.#state,
        [side]: {
          path: listing.path,
          parent: listing.parent,
          entries: sorted(listing.entries),
          loading: false,
          error: undefined,
          selected: keep ? this.#state[side].selected : new Set(),
        },
      });
    } catch (error) {
      this.#setSide(side, { loading: false, error: messageOf(error) });
    }
  }

  /** Reads transfers once a second until none of this session's run. */
  #watch(): void {
    this.#watched += 1;
    if (this.#timer !== undefined) return;
    this.#timer = setInterval(() => void this.#readTransfers(), 1000);
  }

  #stopWatching(): void {
    if (this.#timer !== undefined) clearInterval(this.#timer);
    this.#timer = undefined;
  }

  async #readTransfers(): Promise<void> {
    const session = this.#session;
    if (session === undefined) return;
    if (this.#reading) {
      this.#readAgain = true;
      return;
    }
    this.#reading = true;
    try {
      do {
        this.#readAgain = false;
        // oxlint-disable-next-line no-await-in-loop -- a read asked for during one runs after it
        await this.#readTransfersOnce(session);
      } while (this.#readAgain && this.#session === session);
    } finally {
      this.#reading = false;
    }
  }

  async #readTransfersOnce(session: number): Promise<void> {
    const watched = this.#watched;
    const [all, edits] = await Promise.all([this.#client.transfers(), this.#client.edits()]);
    const mine = all.filter((t) => t.session === session);
    const myEdits = edits.filter((e) => e.session === session);
    const before = new Map(this.#state.transfers.map((t) => [t.id, t.status.kind]));
    const landed = new Set<"local" | "remote">();
    for (const t of mine) {
      const was = before.get(t.id);
      if (t.status.kind === "done" && was !== "done") {
        landed.add(t.direction === "download" ? "local" : "remote");
      }
    }
    this.#set({ ...this.#state, transfers: mine, edits: myEdits });
    const busy = mine.some((t) => t.status.kind === "running" || t.status.kind === "waiting");
    if (!busy && myEdits.length === 0 && watched === this.#watched) {
      this.#stopWatching();
    }
    for (const side of landed) this.#reloadSoon(side);
  }

  /**
   * Lists a side again after a transfer landed there: at once the first
   * time, then at most once every 5 s, since a big folder over a slow link
   * takes seconds to list and many small files land in a burst.
   */
  #reloadSoon(side: "local" | "remote"): void {
    if (this.#pendingReload[side]) return;
    const wait = this.#landed[side] + 5000 - Date.now();
    const reload = () => {
      this.#pendingReload[side] = false;
      this.#landed[side] = Date.now();
      void (side === "local"
        ? this.goLocal(this.#state.local.path)
        : this.goRemote(this.#state.remote.path));
    };
    if (wait <= 0) {
      reload();
      return;
    }
    this.#pendingReload[side] = true;
    setTimeout(reload, wait);
  }

  #ask(prompt: PromptKind, withdrawn?: AbortSignal): Promise<AnswerSpec> {
    this.#state.prompt?.answer(CANCEL);
    return new Promise((resolve) => {
      const pending: PendingPrompt = {
        prompt,
        answer: (reply) => {
          if (this.#state.prompt === pending) this.#set({ ...this.#state, prompt: undefined });
          resolve(reply);
        },
      };
      // No longer asked, as when a security key answered: it goes away.
      withdrawn?.addEventListener("abort", () => pending.answer(CANCEL), { once: true });
      this.#set({ ...this.#state, prompt: pending });
    });
  }

  #setSide(side: "local" | "remote", patch: Partial<Side>): void {
    this.#set({ ...this.#state, [side]: { ...this.#state[side], ...patch } });
  }

  #set(state: FilesState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}
