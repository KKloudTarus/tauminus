// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type Direction,
  type LayoutNode,
  type PaneId,
  type Side,
  type Template,
  balanced,
  closePane,
  countPanes,
  fromTemplate,
  neighbor,
  pane,
  paneIds,
  paneReceivingSpace,
  resizeSplit,
  splitPane,
  templatePanes,
  toTemplate,
} from "./layout.ts";

/** What the workspace needs from a terminal; `TerminalController` satisfies it. */
export interface PaneTerminal {
  getStatus(): { kind: string; code?: unknown };
  subscribe(listener: () => void): () => void;
  focus(): void;
  dispose(): void;
}

export interface Tab {
  readonly id: string;
  readonly root: LayoutNode;
  readonly focused: PaneId;
}

export interface WorkspaceSnapshot {
  readonly tabs: readonly Tab[];
  readonly activeTabId: string | undefined;
}

/** Termius allows 16 panes per tab; more stop being readable on one screen. */
export const MAX_PANES_PER_TAB = 16;
/** Matches the backend's session limit (`SessionConfig::max_sessions`). */
export const MAX_PANES = 64;

/**
 * Tabs, their pane layouts, and the terminal in each pane.
 *
 * Lives outside React: views read immutable snapshots through
 * `useSyncExternalStore`, and every change replaces the snapshot, so a view
 * re-renders only when what it reads has changed.
 *
 * Terminals are created on demand and disposed exactly once, when their pane
 * closes. A pane whose shell exits with code 0 closes itself, like a terminal
 * window; any other exit stays open so its message can be read.
 *
 * Each pane records what it was opened with (`S`, such as an SSH target), so
 * splitting a pane opens the same thing again.
 *
 * Cost: tab operations are O(t) for t tabs; pane operations are O(n) in the
 * tab's layout (n <= 31); finding a pane's tab is O(P) for P <= 64 panes in
 * total; terminal lookup is O(1).
 */
export class Workspace<T extends PaneTerminal = PaneTerminal, S = undefined> {
  readonly #create: (paneId: PaneId, spec: S | undefined) => T;
  readonly #terminals = new Map<
    PaneId,
    { terminal: T; spec: S | undefined; unsubscribe: () => void }
  >();
  readonly #listeners = new Set<() => void>();
  #snapshot: WorkspaceSnapshot = { tabs: [], activeTabId: undefined };
  #nextId = 1;
  #disposed = false;

  constructor(create: (paneId: PaneId, spec: S | undefined) => T) {
    this.#create = create;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): WorkspaceSnapshot => this.#snapshot;

  get activeTab(): Tab | undefined {
    const { tabs, activeTabId } = this.#snapshot;
    return tabs.find((tab) => tab.id === activeTabId);
  }

  terminal(paneId: PaneId): T | undefined {
    return this.#terminals.get(paneId)?.terminal;
  }

  get paneCount(): number {
    return this.#terminals.size;
  }

  /** What the focused pane of the active tab was opened with. O(t). */
  focusedSpec(): S | undefined {
    const tab = this.#snapshot.tabs.find((t) => t.id === this.#snapshot.activeTabId);
    return tab ? this.#terminals.get(tab.focused)?.spec : undefined;
  }

  /** Opens a tab with one pane and makes it active. Returns false at the pane limit. */
  newTab(spec?: S): boolean {
    if (this.#disposed || this.#terminals.size >= MAX_PANES) {
      return false;
    }
    const paneId = this.#newPane(spec);
    const tab: Tab = { id: this.#id("tab"), root: pane(paneId), focused: paneId };
    this.#update({ tabs: [...this.#snapshot.tabs, tab], activeTabId: tab.id });
    return true;
  }

  /**
   * Opens `template` as a new active tab. Returns false, opening nothing,
   * when it holds no pane or would pass a pane limit.
   */
  openTemplate(template: Template<S | undefined>): boolean {
    const count = templatePanes(template);
    if (
      this.#disposed ||
      count === 0 ||
      count > MAX_PANES_PER_TAB ||
      this.#terminals.size + count > MAX_PANES
    ) {
      return false;
    }
    const root = fromTemplate(
      template,
      (spec) => this.#newPane(spec),
      () => this.#id("split"),
    );
    if (!root) return false;
    const [first] = paneIds(root);
    if (!first) return false;
    const tab: Tab = { id: this.#id("tab"), root, focused: first };
    this.#update({ tabs: [...this.#snapshot.tabs, tab], activeTabId: tab.id });
    return true;
  }

  /** Each tab's layout with what its panes were opened with, in tab order. O(P). */
  templates(): Template<S | undefined>[] {
    return this.#snapshot.tabs.map((tab) =>
      toTemplate(tab.root, (id) => this.#terminals.get(id)?.spec),
    );
  }

  /** Closes a tab and disposes its terminals. The next tab, or the previous one, becomes active. */
  closeTab(tabId: string): void {
    const { tabs, activeTabId } = this.#snapshot;
    const index = tabs.findIndex((tab) => tab.id === tabId);
    const tab = tabs[index];
    if (!tab) {
      return;
    }
    const remaining = tabs.filter((t) => t.id !== tabId);
    const nextActive =
      activeTabId === tabId ? (remaining[index] ?? remaining[index - 1])?.id : activeTabId;
    this.#update({ tabs: remaining, activeTabId: nextActive });
    for (const paneId of paneIds(tab.root)) {
      this.#disposePane(paneId);
    }
  }

  activateTab(tabId: string): void {
    if (this.#snapshot.tabs.some((tab) => tab.id === tabId)) {
      this.#update({ ...this.#snapshot, activeTabId: tabId });
    }
  }

  /** Activates the tab `delta` places away, wrapping around. */
  cycleTab(delta: number): void {
    const { tabs, activeTabId } = this.#snapshot;
    if (tabs.length === 0) return;
    const index = tabs.findIndex((tab) => tab.id === activeTabId);
    const next = tabs[(((index + delta) % tabs.length) + tabs.length) % tabs.length];
    if (next) this.activateTab(next.id);
  }

  /** Moves a tab to position `to`, clamped to the tab list. */
  moveTab(tabId: string, to: number): void {
    const tabs = [...this.#snapshot.tabs];
    const from = tabs.findIndex((tab) => tab.id === tabId);
    if (from < 0) return;
    if (!Number.isFinite(to)) return;
    const target = Math.max(0, Math.min(tabs.length - 1, Math.trunc(to)));
    if (target === from) return;
    const [tab] = tabs.splice(from, 1);
    if (tab) tabs.splice(target, 0, tab);
    this.#update({ ...this.#snapshot, tabs });
  }

  /** Moves the active tab `delta` places. */
  moveActiveTab(delta: number): void {
    const { tabs, activeTabId } = this.#snapshot;
    const index = tabs.findIndex((tab) => tab.id === activeTabId);
    if (index >= 0 && activeTabId) this.moveTab(activeTabId, index + delta);
  }

  /**
   * Splits the active tab's focused pane, opening `spec`, or what that pane
   * was opened with. The new pane gets focus. Returns false at the per-tab
   * or total pane limit.
   */
  split(direction: Direction, spec?: S): boolean {
    const tab = this.activeTab;
    if (!tab || countPanes(tab.root) >= MAX_PANES_PER_TAB || this.#terminals.size >= MAX_PANES) {
      return false;
    }
    const paneId = this.#newPane(spec ?? this.#terminals.get(tab.focused)?.spec);
    const root = splitPane(tab.root, tab.focused, direction, paneId, this.#id("split"));
    this.#replaceTab({ ...tab, root, focused: paneId });
    return true;
  }

  /** Closes a pane, the focused one by default. Closing the last pane closes its tab. */
  closePane(paneId?: PaneId): void {
    const tab = paneId ? this.#tabOf(paneId) : this.activeTab;
    const target = paneId ?? tab?.focused;
    if (!tab || !target) return;
    const root = closePane(tab.root, target);
    if (!root) {
      this.closeTab(tab.id);
      return;
    }
    const remaining = paneIds(root);
    // Focus follows the pane that grows into the freed space.
    const focused =
      tab.focused === target
        ? (paneReceivingSpace(tab.root, target) ?? remaining[0] ?? tab.focused)
        : tab.focused;
    this.#replaceTab({ ...tab, root, focused });
    this.#disposePane(target);
  }

  focusPane(paneId: PaneId): void {
    const tab = this.#tabOf(paneId);
    if (!tab) return;
    if (tab.focused !== paneId || this.#snapshot.activeTabId !== tab.id) {
      this.#update({
        tabs: this.#snapshot.tabs.map((t) => (t.id === tab.id ? { ...t, focused: paneId } : t)),
        activeTabId: tab.id,
      });
    }
    this.terminal(paneId)?.focus();
  }

  /** Moves focus to the pane on `side` of the focused one, if there is one. */
  focusNeighbor(side: Side): void {
    const tab = this.activeTab;
    if (!tab) return;
    const next = neighbor(tab.root, tab.focused, side);
    if (next) this.focusPane(next);
  }

  /** Gives every pane of a split in the active tab the same share. */
  balance(): void {
    const tab = this.activeTab;
    if (tab) this.#replaceTab({ ...tab, root: balanced(tab.root) });
  }

  resize(tabId: string, splitId: string, sizes: readonly number[]): void {
    const tab = this.#snapshot.tabs.find((t) => t.id === tabId);
    if (!tab) return;
    const root = resizeSplit(tab.root, splitId, sizes);
    if (root !== tab.root) this.#replaceTab({ ...tab, root });
  }

  forEachTerminal(visit: (terminal: T) => void): void {
    for (const { terminal } of this.#terminals.values()) visit(terminal);
  }

  dispose(): void {
    this.#disposed = true;
    // Deleting the current entry while iterating a Map is safe in JavaScript.
    for (const paneId of this.#terminals.keys()) {
      this.#disposePane(paneId);
    }
    this.#update({ tabs: [], activeTabId: undefined });
    this.#listeners.clear();
  }

  #newPane(spec: S | undefined): PaneId {
    const paneId = this.#id("pane");
    const terminal = this.#create(paneId, spec);
    const unsubscribe = terminal.subscribe(() => {
      const status = terminal.getStatus();
      if (status.kind === "exited" && status.code === 0) {
        // Close after the current notification finishes.
        queueMicrotask(() => this.closePane(paneId));
      }
    });
    this.#terminals.set(paneId, { terminal, spec, unsubscribe });
    return paneId;
  }

  #disposePane(paneId: PaneId): void {
    const entry = this.#terminals.get(paneId);
    if (!entry) return;
    this.#terminals.delete(paneId);
    entry.unsubscribe();
    entry.terminal.dispose();
  }

  #tabOf(paneId: PaneId): Tab | undefined {
    return this.#snapshot.tabs.find((tab) => paneIds(tab.root).includes(paneId));
  }

  #replaceTab(next: Tab): void {
    this.#update({
      ...this.#snapshot,
      tabs: this.#snapshot.tabs.map((tab) => (tab.id === next.id ? next : tab)),
    });
  }

  #update(snapshot: WorkspaceSnapshot): void {
    this.#snapshot = snapshot;
    for (const listener of this.#listeners) listener();
  }

  #id(prefix: string): string {
    const id = `${prefix}-${this.#nextId}`;
    this.#nextId += 1;
    return id;
  }
}
