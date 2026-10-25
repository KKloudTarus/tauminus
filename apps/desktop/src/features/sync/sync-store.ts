// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { ReviewView, SyncClient, SyncStatusView, TargetInput } from "@/ipc/index.ts";

export interface SyncState {
  /** `undefined` until the first status arrives. */
  readonly status: SyncStatusView | undefined;
  /** The first hosts another device changed, waiting for the user. */
  readonly reviews: readonly ReviewView[];
  /** How many hosts are noted in all. */
  readonly noted: number;
}

const EMPTY: SyncState = { status: undefined, reviews: [], noted: 0 };

/**
 * Sync as the UI sees it, outside React. Each change asks the backend and
 * shows what it answers; the passphrase passes through and is not kept.
 */
export class SyncStore {
  readonly #client: SyncClient;
  readonly #synced: () => void;
  readonly #listeners = new Set<() => void>();
  #state: SyncState = EMPTY;
  #listening = false;
  #lastRun: number | undefined;

  /** `synced` hears of each run that changed the saved lists. */
  constructor(client: SyncClient, synced: () => void = () => {}) {
    this.#client = client;
    this.#synced = synced;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): SyncState => this.#state;

  /** Follows the scheduled runs, once. */
  async listen(): Promise<void> {
    if (this.#listening) return;
    this.#listening = true;
    await this.#client.listen((status) => {
      void this.#reviewsAfter(status).catch((error: unknown) =>
        console.warn("sync reviews failed", error),
      );
    });
  }

  async refresh(): Promise<void> {
    await this.#reviewsAfter(await this.#client.status());
  }

  async create(target: TargetInput, passphrase: string): Promise<void> {
    await this.#reviewsAfter(await this.#client.create(target, passphrase));
  }

  async join(target: TargetInput, passphrase: string): Promise<void> {
    await this.#reviewsAfter(await this.#client.join(target, passphrase));
  }

  /** Syncs now; a failure is in the status as well as thrown. */
  async now(): Promise<void> {
    try {
      await this.#reviewsAfter(await this.#client.now());
    } catch (error) {
      await this.refresh().catch(() => undefined);
      throw error;
    }
  }

  async leave(): Promise<void> {
    await this.#reviewsAfter(await this.#client.leave());
  }

  /** Accepts `reviews` as shown, and resolves to the ids of those that
   * changed since; the list then shows them as they are now. */
  async accept(reviews: readonly ReviewView[]): Promise<string[]> {
    const stale = await this.#client.accept(
      reviews.map(({ host, fingerprint }) => ({ host, fingerprint })),
    );
    await this.#reload();
    return stale;
  }

  chooseFolder(): Promise<string | null> {
    return this.#client.chooseFolder();
  }

  async #reviewsAfter(status: SyncStatusView): Promise<void> {
    // A run that saved something: failed ones too, after saving hosts.
    const run = status.last?.changed ? (status.last.at ?? undefined) : undefined;
    if (run !== undefined && run !== this.#lastRun) {
      this.#lastRun = run;
      this.#synced();
    }
    this.#set({ ...this.#state, status });
    await this.#reload();
  }

  async #reload(): Promise<void> {
    const { reviews, noted } = await this.#client.reviews();
    this.#set({ ...this.#state, reviews, noted });
  }

  #set(state: SyncState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}
