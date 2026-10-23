// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { TriggerSetInput, TriggerSetView, TriggersClient } from "@/ipc/index.ts";

export interface TriggersState {
  /** By name. */
  readonly sets: readonly TriggerSetView[];
  readonly loaded: boolean;
  /** Why the last load or change failed, if it did. */
  readonly error: string | undefined;
}

/**
 * The highlight sets as the UI sees them, outside React. Every change
 * returns the whole list from the backend, so the view never drifts from
 * what was saved; `onChange` runs after each saved change, for panes to
 * match with the new rules. O(S) per change, S ≤ 200.
 */
export class TriggersStore {
  readonly #client: TriggersClient;
  readonly #onChange: () => void;
  readonly #listeners = new Set<() => void>();
  #state: TriggersState = { sets: [], loaded: false, error: undefined };

  constructor(client: TriggersClient, onChange: () => void = () => {}) {
    this.#client = client;
    this.#onChange = onChange;
  }

  get client(): TriggersClient {
    return this.#client;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): TriggersState => this.#state;

  async load(): Promise<void> {
    await this.#apply(() => this.#client.list(), false);
  }

  /** True when it was saved; the error is in the state otherwise. */
  save(set: TriggerSetInput): Promise<boolean> {
    return this.#apply(() => this.#client.save(set));
  }

  remove(id: string): Promise<boolean> {
    return this.#apply(() => this.#client.remove(id));
  }

  /**
   * Makes `id` apply to every session, last in precedence, or stop to; the
   * others keep their order.
   */
  setDefault(id: string, on: boolean): Promise<boolean> {
    const ids = defaults(this.#state.sets).filter((other) => other !== id);
    if (on) ids.push(id);
    return this.#apply(() => this.#client.saveDefaults(ids));
  }

  async #apply(change: () => Promise<TriggerSetView[]>, changes = true): Promise<boolean> {
    try {
      const sets = await change();
      this.#set({ sets, loaded: true, error: undefined });
      if (changes) this.#onChange();
      return true;
    } catch (error) {
      this.#set({ ...this.#state, error: messageOf(error) });
      return false;
    }
  }

  #set(state: TriggersState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}

/** The ids of the sets every session gets, first in precedence first. O(S log S). */
export function defaults(sets: readonly TriggerSetView[]): string[] {
  return sets
    .flatMap((s) => (s.rank === null || s.rank === undefined ? [] : [{ id: s.id, rank: s.rank }]))
    .toSorted((a, b) => a.rank - b.rank)
    .map((s) => s.id);
}
