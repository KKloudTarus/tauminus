// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { PaneId } from "./layout.ts";

/**
 * The panes that type together: what the user types in one of them goes
 * to the others too. Outside React; at most `MAX_PANES` ids.
 */
export class Broadcast {
  #panes: ReadonlySet<PaneId> = new Set();
  readonly #listeners = new Set<() => void>();

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): ReadonlySet<PaneId> => this.#panes;

  has(pane: PaneId): boolean {
    return this.#panes.has(pane);
  }

  /** Adds `pane`, or takes it out. */
  toggle(pane: PaneId): void {
    const next = new Set(this.#panes);
    if (!next.delete(pane)) next.add(pane);
    this.#set(next);
  }

  /** Exactly these panes, or none when given fewer than two: one pane has no one to type to. */
  set(panes: Iterable<PaneId>): void {
    const next = new Set(panes);
    this.#set(next.size < 2 ? new Set() : next);
  }

  remove(pane: PaneId): void {
    if (!this.#panes.has(pane)) return;
    const next = new Set(this.#panes);
    next.delete(pane);
    this.#set(next);
  }

  clear(): void {
    if (this.#panes.size > 0) this.#set(new Set());
  }

  /** The others `from` types into: none unless `from` is one of them. O(P). */
  targets(from: PaneId): PaneId[] {
    return this.#panes.has(from) ? [...this.#panes].filter((p) => p !== from) : [];
  }

  #set(panes: ReadonlySet<PaneId>): void {
    this.#panes = panes;
    for (const listener of this.#listeners) listener();
  }
}

/**
 * Replies the terminal sends for itself, which belong to one pane: cursor,
 * device, mode (DECRPM), and window reports, focus in and out, mouse reports, and OSC or DCS
 * answers. Everything else came from the user's keys or a paste.
 */
const TERMINAL_REPLY =
  // oxlint-disable-next-line no-control-regex -- the replies are escape sequences
  /^(?:\u001b\[[?>]?[\d;]*(?:[Rcnt]|\$y)|\u001b\[[IO]|\u001b\[<[\d;]+[Mm]|\u001b\[M[\s\S]{3}|\u001b\][\s\S]*(?:\u0007|\u001b\\)|\u001bP[\s\S]*\u001b\\)$/u;

export function typedByUser(data: string): boolean {
  return !TERMINAL_REPLY.test(data);
}
