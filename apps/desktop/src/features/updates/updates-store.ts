// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { UpdateView, UpdatesClient } from "@/ipc/index.ts";

/** Whether the app looks for a new version when it starts. */
const AUTO_KEY = "tauminus.updates.auto";
/** When it last looked, in milliseconds since the epoch. */
const LAST_KEY = "tauminus.updates.last";
/** At most one look a day at start. */
const DAY = 24 * 60 * 60 * 1000;

export interface UpdatesState {
  readonly status: "idle" | "checking" | "current" | "available" | "installing" | "failed";
  readonly update?: UpdateView | undefined;
  readonly error?: string | undefined;
  /** How much of the download has come, while installing. */
  readonly percent?: number | undefined;
  /** What failed, when something did. */
  readonly failed?: "check" | "install" | undefined;
  /** Whether to look when the app starts. */
  readonly auto: boolean;
  /** The user asked; a quiet check at start says nothing unless it finds a version. */
  readonly asked: boolean;
  readonly dismissed: boolean;
}

/**
 * New versions, outside React. A check at start runs at most once a day
 * and only while `auto` is on; nothing installs until the user asks.
 */
export class UpdatesStore {
  readonly #client: UpdatesClient;
  readonly #storage: Storage | undefined;
  readonly #now: () => number;
  readonly #listeners = new Set<() => void>();
  #state: UpdatesState;

  constructor(client: UpdatesClient, storage: Storage | undefined, now: () => number = Date.now) {
    this.#client = client;
    this.#storage = storage;
    this.#now = now;
    this.#state = {
      status: "idle",
      auto: read(storage, AUTO_KEY) !== "0",
      asked: false,
      dismissed: false,
    };
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): UpdatesState => this.#state;

  /** Looks once at start, when on and the last look is a day old. */
  async startup(): Promise<void> {
    if (!this.#state.auto) return;
    const last = Number(read(this.#storage, LAST_KEY) ?? 0);
    const now = this.#now();
    // A clock set back counts as a day gone.
    if (Number.isFinite(last) && now >= last && now - last < DAY) return;
    await this.check(false);
  }

  /** Looks for a new version; `asked` when the user did, so the answer shows either way. */
  async check(asked = true): Promise<void> {
    if (this.#state.status === "checking") {
      // The check running answers the user too.
      if (asked && !this.#state.asked) this.#set({ ...this.#state, asked: true });
      return;
    }
    if (this.#state.status === "installing") return;
    this.#set({ ...this.#state, status: "checking", asked, dismissed: false, error: undefined });
    try {
      const update = await this.#client.check();
      write(this.#storage, LAST_KEY, String(this.#now()));
      this.#set({
        ...this.#state,
        status: update ? "available" : "current",
        update: update ?? undefined,
      });
    } catch (error) {
      this.#set({ ...this.#state, status: "failed", failed: "check", error: messageOf(error) });
    }
  }

  /** Installs what the last check found; the app restarts into it. */
  async install(): Promise<void> {
    if (this.#state.status !== "available") return;
    this.#set({ ...this.#state, status: "installing", percent: undefined, error: undefined });
    try {
      await this.#client.install((percent) => {
        if (this.#state.status === "installing") this.#set({ ...this.#state, percent });
      });
    } catch (error) {
      this.#set({
        ...this.#state,
        status: "failed",
        failed: "install",
        asked: true,
        error: messageOf(error),
      });
    }
  }

  setAuto(auto: boolean): void {
    write(this.#storage, AUTO_KEY, auto ? "1" : "0");
    this.#set({ ...this.#state, auto });
  }

  dismiss(): void {
    this.#set({ ...this.#state, dismissed: true });
  }

  #set(state: UpdatesState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}

/** What the banner says, or nothing to show. */
export function updateNotice(state: UpdatesState): string | undefined {
  if (state.dismissed) return undefined;
  switch (state.status) {
    case "available":
      return state.update
        ? `Tauminus ${state.update.version} is available; this is ${state.update.current}.`
        : undefined;
    case "installing":
      return state.percent === undefined || state.percent >= 100
        ? `Installing Tauminus ${state.update?.version ?? ""}; it restarts when done.`
        : `Downloading Tauminus ${state.update?.version ?? ""}: ${state.percent}%`;
    case "current":
      return state.asked ? "This is the newest version of Tauminus." : undefined;
    case "failed": {
      if (!state.asked) return undefined;
      const what = state.failed === "install" ? "install the update" : "check for updates";
      return `Could not ${what}: ${state.error ?? "unknown error"}`;
    }
    case "checking":
      return state.asked ? "Checking for updates…" : undefined;
    default:
      return undefined;
  }
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function read(storage: Storage | undefined, key: string): string | null {
  try {
    return storage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function write(storage: Storage | undefined, key: string, value: string): void {
  try {
    storage?.setItem(key, value);
  } catch {
    // Private mode: the choice lasts until the app closes.
  }
}
