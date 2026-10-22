// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type IdentityEdit,
  type IdentityView,
  type PromptHandler,
  SessionCommandError,
  type VaultClient,
  type VaultStatusSpec,
} from "@/ipc/index.ts";

export interface VaultState {
  /** `undefined` until the first status arrives. */
  readonly status: VaultStatusSpec | undefined;
  /** Empty while locked. */
  readonly identities: readonly IdentityView[];
}

/** Activity reaches the backend's idle timer at most this often, or every
 * quarter of the idle time when that is shorter. */
export const TOUCH_EVERY_MS = 30_000;
/** The idle time the user chose; the backend starts at its default. */
export const IDLE_STORAGE_KEY = "tauminus.vault.idleMinutes";

const EMPTY: VaultState = { status: undefined, identities: [] };

/**
 * The vault as the UI sees it, outside React. Each change asks the backend
 * and then reloads the status and the identity list, so the view never shows
 * a state the backend does not have. Secrets pass through on their way in and
 * are not kept.
 */
export class VaultStore {
  readonly #client: VaultClient;
  readonly #now: () => number;
  readonly #storage: Storage | undefined;
  #restored = false;
  readonly #listeners = new Set<() => void>();
  #state: VaultState = EMPTY;
  #touched = Number.NEGATIVE_INFINITY;

  constructor(client: VaultClient, now: () => number = () => performance.now(), storage?: Storage) {
    this.#client = client;
    this.#now = now;
    this.#storage = storage;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): VaultState => this.#state;

  async refresh(): Promise<void> {
    await this.#restoreIdle();
    const status = await this.#client.status();
    // The vault may have locked itself on the idle timer.
    const identities = status.unlocked ? await this.#identities() : [];
    this.#set({ status, identities });
  }

  async create(password: string): Promise<void> {
    await this.#client.create(password);
    await this.refresh();
  }

  async unlock(password: string): Promise<void> {
    await this.#client.unlock(password);
    await this.refresh();
  }

  async unlockWithKeychain(): Promise<void> {
    await this.#client.unlockWithKeychain();
    await this.refresh();
  }

  async lock(): Promise<void> {
    await this.#client.lock();
    await this.refresh();
  }

  saveIdentity(edit: IdentityEdit): Promise<string> {
    return this.#thenRefresh(this.#client.saveIdentity(edit));
  }

  deleteIdentity(id: string): Promise<void> {
    return this.#thenRefresh(this.#client.deleteIdentity(id));
  }

  async setIdle(minutes: number): Promise<void> {
    await this.#client.setIdle(minutes);
    try {
      this.#storage?.setItem(IDLE_STORAGE_KEY, String(minutes));
    } catch (error) {
      console.warn("could not remember the vault idle time", error);
    }
    await this.refresh();
  }

  /** Adds an identity's public key to a saved host's authorized_keys. */
  installKey(identity: string, host: string, onPrompt: PromptHandler): Promise<void> {
    return this.#thenRefresh(this.#client.installKey(identity, host, onPrompt));
  }

  setKeychain(on: boolean, password?: string): Promise<void> {
    return this.#thenRefresh(this.#client.setKeychain(on, password));
  }

  setPresence(on: boolean): Promise<void> {
    return this.#thenRefresh(this.#client.setPresence(on));
  }

  changePassword(current: string, next: string): Promise<string | null> {
    return this.#thenRefresh(this.#client.changePassword(current, next));
  }

  /**
   * Runs `retry` at once when the backend says the vault is open (another
   * pane unlocked it), or calls `ask` to have the user unlock it first.
   */
  async retryOrAsk(retry: () => void, ask: () => void): Promise<void> {
    await this.refresh().catch((error: unknown) => console.warn("vault status failed", error));
    if (this.#state.status?.unlocked) retry();
    else ask();
  }

  /** The user did something: keeps an open vault from locking. Throttled. */
  activity(): void {
    if (!this.#state.status?.unlocked) return;
    const now = this.#now();
    const every = Math.min(TOUCH_EVERY_MS, (this.#state.status.idleMinutes * 60_000) / 4);
    if (now - this.#touched < every) return;
    this.#touched = now;
    this.#client.touch().catch((error: unknown) => console.warn("vault touch failed", error));
  }

  /**
   * Reloads after `work`, failed or not: a failed save locks the vault, and
   * the view must show that. The work's own error is what the caller sees.
   */
  async #thenRefresh<T>(work: Promise<T>): Promise<T> {
    try {
      return await work;
    } finally {
      await this.refresh().catch((error: unknown) => console.warn("vault status failed", error));
    }
  }

  /** Gives the backend the idle time chosen in an earlier run, once. */
  async #restoreIdle(): Promise<void> {
    if (this.#restored) return;
    this.#restored = true;
    let saved: number | undefined;
    try {
      const raw = this.#storage?.getItem(IDLE_STORAGE_KEY);
      saved = raw ? Number(raw) : undefined;
    } catch {
      saved = undefined;
    }
    if (saved === undefined || !Number.isInteger(saved)) return;
    await this.#client
      .setIdle(saved)
      .catch((error: unknown) => console.warn("ignoring the saved vault idle time", error));
  }

  /** The list, or empty when the vault locked between the two calls. */
  async #identities(): Promise<IdentityView[]> {
    try {
      return await this.#client.identities();
    } catch (error) {
      if (error instanceof SessionCommandError && error.code === "vault_locked") return [];
      throw error;
    }
  }

  #set(state: VaultState): void {
    // A focus with nothing new must not re-render the host tree. O(i).
    if (sameState(this.#state, state)) return;
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}

function sameState(a: VaultState, b: VaultState): boolean {
  const x = a.status;
  const y = b.status;
  if (x === undefined || y === undefined) return x === y && a.identities.length === 0;
  return (
    x.exists === y.exists &&
    x.unlocked === y.unlocked &&
    x.idleMinutes === y.idleMinutes &&
    JSON.stringify(x.keychain) === JSON.stringify(y.keychain) &&
    JSON.stringify(x.presence) === JSON.stringify(y.presence) &&
    a.identities.length === b.identities.length &&
    a.identities.every((identity, n) => sameIdentity(identity, b.identities[n]))
  );
}

function sameIdentity(a: IdentityView, b: IdentityView | undefined): boolean {
  return (
    b !== undefined &&
    a.id === b.id &&
    a.name === b.name &&
    a.user === b.user &&
    a.hasPassword === b.hasPassword &&
    a.hasKey === b.hasKey &&
    a.publicKey === b.publicKey
  );
}
