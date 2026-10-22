// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { GuardSource } from "@/features/terminal/command-guard.ts";
import type { GroupInput, HostInput, HostView, HostsClient, HostsView } from "@/ipc/index.ts";

export interface HostsState {
  readonly view: HostsView;
  readonly status: "loading" | "ready" | "failed";
  /** Why the list could not load; the sidebar shows it instead of hosts. */
  readonly error: string | undefined;
  /** Hosts `~/.ssh/config` names, for the import offer. */
  readonly configAliases: number;
  /** The result of the last import, until dismissed. */
  readonly notice: string | undefined;
}

const EMPTY: HostsView = { groups: [], hosts: [] };

/**
 * The saved hosts as the UI sees them, outside React. Each change goes to
 * the backend and the list it returns replaces this one, so the view never
 * shows a change that was not saved. Lookups by id are O(1) through maps
 * rebuilt once per list.
 */
export class HostsStore {
  readonly #client: HostsClient;
  readonly #listeners = new Set<() => void>();
  #state: HostsState = {
    view: EMPTY,
    status: "loading",
    error: undefined,
    configAliases: 0,
    notice: undefined,
  };
  #groupNames = new Map<string, string>();
  #guarded = new Map<string, boolean>();

  constructor(client: HostsClient) {
    this.#client = client;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): HostsState => this.#state;

  async load(): Promise<void> {
    try {
      const [view, configAliases] = await Promise.all([
        this.#client.list(),
        this.#client.configAliases().catch((error: unknown) => {
          // The import offer is optional; the list still loads.
          console.warn("could not read ~/.ssh/config", error);
          return 0;
        }),
      ]);
      this.#setView(view, { status: "ready", error: undefined, configAliases });
    } catch (error) {
      this.#set({ ...this.#state, status: "failed", error: messageOf(error) });
    }
  }

  /**
   * The prod guard for a terminal connected to `hostId`. It follows the
   * host's tags and settings as they change. A host deleted while connected
   * keeps the guard it last had.
   */
  guardFor(hostId: string): GuardSource {
    let last = this.#guarded.get(hostId) ?? false;
    return {
      active: () => {
        last = this.#guarded.get(hostId) ?? last;
        return last;
      },
      subscribe: this.subscribe,
      check: (lines) => this.#client.checkCommand(hostId, lines),
    };
  }

  /** The host's group name, or "" when it has none. */
  groupName = (host: HostView): string =>
    host.group ? (this.#groupNames.get(host.group) ?? "") : "";

  async saveHost(host: HostInput): Promise<void> {
    this.#setView(await this.#client.saveHost(host));
  }

  async deleteHost(id: string): Promise<void> {
    this.#setView(await this.#client.deleteHost(id));
  }

  async saveGroup(group: GroupInput): Promise<void> {
    this.#setView(await this.#client.saveGroup(group));
  }

  async deleteGroup(id: string): Promise<void> {
    this.#setView(await this.#client.deleteGroup(id));
  }

  async importConfig(): Promise<void> {
    const result = await this.#client.importConfig();
    const skipped = result.skipped.length;
    const notice =
      `Imported ${result.added} ${result.added === 1 ? "host" : "hosts"} from ~/.ssh/config` +
      (skipped > 0
        ? `; skipped ${skipped}: ${result.skipped.map((s) => `${s.alias} (${s.reason})`).join(", ")}`
        : "");
    this.#setView(result.view, { notice });
  }

  /** Shows `text` where import results appear, for example why a host cannot connect. */
  notify(text: string): void {
    this.#set({ ...this.#state, notice: text });
  }

  dismissNotice(): void {
    if (this.#state.notice !== undefined) this.#set({ ...this.#state, notice: undefined });
  }

  #setView(view: HostsView, extra: Partial<HostsState> = {}): void {
    this.#groupNames = new Map(view.groups.map((g) => [g.id, g.name]));
    this.#guarded = new Map(view.hosts.map((h) => [h.id, h.guarded]));
    this.#set({ ...this.#state, ...extra, view });
  }

  #set(state: HostsState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}

export function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
