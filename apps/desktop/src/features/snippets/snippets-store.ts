// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { SnippetInput, SnippetView, SnippetsClient } from "@/ipc/index.ts";

export interface SnippetsState {
  /** By folder, then name. */
  readonly snippets: readonly SnippetView[];
  readonly loaded: boolean;
  /** Why the last load or change failed, if it did. */
  readonly error: string | undefined;
}

/** A folder and the snippets in it; `null` holds those in none. */
export interface Folder {
  readonly name: string | null;
  readonly snippets: readonly SnippetView[];
}

/**
 * The snippet library as the UI sees it, outside React. Every change
 * returns the whole list from the backend, so the view never drifts from
 * what was saved. O(S) per change, S ≤ 2000.
 */
export class SnippetsStore {
  readonly #client: SnippetsClient;
  readonly #listeners = new Set<() => void>();
  #state: SnippetsState = { snippets: [], loaded: false, error: undefined };

  constructor(client: SnippetsClient) {
    this.#client = client;
  }

  get client(): SnippetsClient {
    return this.#client;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): SnippetsState => this.#state;

  find(id: string): SnippetView | undefined {
    return this.#state.snippets.find((s) => s.id === id);
  }

  async load(): Promise<void> {
    await this.#apply(() => this.#client.list());
  }

  /** True when it was saved; the error is in the state otherwise. */
  save(snippet: SnippetInput): Promise<boolean> {
    return this.#apply(() => this.#client.save(snippet));
  }

  remove(id: string): Promise<boolean> {
    return this.#apply(() => this.#client.remove(id));
  }

  async #apply(change: () => Promise<SnippetView[]>): Promise<boolean> {
    try {
      const snippets = await change();
      this.#set({ snippets, loaded: true, error: undefined });
      return true;
    } catch (error) {
      this.#set({ ...this.#state, error: messageOf(error) });
      return false;
    }
  }

  #set(state: SnippetsState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}

/** Groups a sorted list by folder, unfiled first. O(S). */
export function byFolder(snippets: readonly SnippetView[]): Folder[] {
  const folders: { name: string | null; snippets: SnippetView[] }[] = [];
  for (const snippet of snippets) {
    const last = folders.at(-1);
    if (last && last.name === snippet.folder) {
      last.snippets.push(snippet);
    } else {
      folders.push({ name: snippet.folder, snippets: [snippet] });
    }
  }
  return folders;
}

/** How a snippet reads in a list: its folder, then its name. */
export function snippetTitle(snippet: SnippetView): string {
  return snippet.folder ? `${snippet.folder} / ${snippet.name}` : snippet.name;
}
