// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type Action,
  type Keymap,
  type Platform,
  buildKeymap,
  defaultBindings,
  detectPlatform,
} from "@/features/keymap/keymap.ts";
import { TerminalController } from "@/features/terminal/controller.ts";
import type { RendererPreference } from "@/features/terminal/renderer.ts";
import { Workspace } from "@/features/workspace/workspace.ts";
import type { SessionClient, SessionSpec } from "@/ipc/index.ts";

import { type ThemePreference, applyTheme, loadTheme, nextTheme, saveTheme } from "./theme.ts";

const KEYMAP_STORAGE_KEY = "tauminus.keymap";

export const ACTION_LABELS: Record<Action, string> = {
  "palette.open": "Show all commands",
  "tab.new": "New tab",
  "tab.close": "Close tab",
  "tab.next": "Next tab",
  "tab.previous": "Previous tab",
  "tab.moveLeft": "Move tab left",
  "tab.moveRight": "Move tab right",
  "pane.splitRight": "Split pane right",
  "pane.splitDown": "Split pane down",
  "pane.close": "Close pane",
  "pane.focusLeft": "Focus pane on the left",
  "pane.focusRight": "Focus pane on the right",
  "pane.focusUp": "Focus pane above",
  "pane.focusDown": "Focus pane below",
  "terminal.copy": "Copy selection",
  "terminal.paste": "Paste",
  "terminal.clear": "Clear scrollback",
  "theme.cycle": "Switch theme (system, light, dark)",
  "ssh.connect": "Connect to SSH host",
};

interface ShellState {
  paletteOpen: boolean;
  connectOpen: boolean;
  theme: ThemePreference;
}

/**
 * The app outside React: the workspace, the keymap, and UI state that
 * shortcuts change. One `run` dispatches every action, whether it came from a
 * key, the palette, or a button.
 */
export class AppShell {
  readonly workspace: Workspace<TerminalController, SessionSpec>;
  readonly keymap: Keymap;
  readonly platform: Platform;
  readonly #listeners = new Set<() => void>();
  #state: ShellState;

  constructor(options: {
    sessions: SessionClient;
    userAgent: string;
    storage: Storage | undefined;
    root: HTMLElement;
    clipboard: Pick<Clipboard, "readText" | "writeText"> | undefined;
    renderer?: RendererPreference;
  }) {
    this.platform = detectPlatform(options.userAgent);
    this.#storage = options.storage;
    this.#root = options.root;
    this.#clipboard = options.clipboard;
    const { keymap, problems } = buildKeymap(
      defaultBindings(this.platform),
      readOverrides(options.storage),
    );
    for (const problem of problems) {
      console.warn(
        `ignoring key binding ${problem.binding} for ${problem.action}: ${problem.reason}`,
      );
    }
    this.keymap = keymap;
    this.#state = { paletteOpen: false, connectOpen: false, theme: loadTheme(options.storage) };
    applyTheme(options.root, this.#state.theme);

    this.workspace = new Workspace<TerminalController, SessionSpec>((_paneId, spec) => {
      const controller = new TerminalController(options.sessions, {
        ...(options.renderer ? { renderer: options.renderer } : {}),
        ...(spec ? { spec } : {}),
      });
      controller.setKeyHandler((event) => this.handleKey(event));
      return controller;
    });
  }

  readonly #storage: Storage | undefined;
  readonly #root: HTMLElement;
  readonly #clipboard: Pick<Clipboard, "readText" | "writeText"> | undefined;

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getState = (): ShellState => this.#state;

  /** Runs the action bound to a key event. Returns true when it handled the key. */
  handleKey(event: KeyboardEvent): boolean {
    const action = this.keymap.match(event);
    if (!action) {
      return false;
    }
    event.preventDefault();
    this.run(action);
    return true;
  }

  setPaletteOpen(open: boolean): void {
    if (this.#state.paletteOpen !== open) {
      this.#set({ ...this.#state, paletteOpen: open });
      if (!open) this.#focusActivePane();
    }
  }

  setConnectOpen(open: boolean): void {
    if (this.#state.connectOpen !== open) {
      this.#set({ ...this.#state, connectOpen: open });
      if (!open) this.#focusActivePane();
    }
  }

  /** Opens an SSH session in a new tab. Returns false at the pane limit. */
  connect(spec: SessionSpec): boolean {
    const opened = this.workspace.newTab(spec);
    if (opened) this.#focusActivePane();
    return opened;
  }

  run(action: Action): void {
    const w = this.workspace;
    switch (action) {
      case "palette.open":
        this.setPaletteOpen(true);
        return;
      case "ssh.connect":
        this.setConnectOpen(true);
        return;
      case "tab.new":
        w.newTab();
        break;
      case "tab.close": {
        const tab = w.activeTab;
        if (tab) w.closeTab(tab.id);
        break;
      }
      case "tab.next":
        w.cycleTab(1);
        break;
      case "tab.previous":
        w.cycleTab(-1);
        break;
      case "tab.moveLeft":
        w.moveActiveTab(-1);
        break;
      case "tab.moveRight":
        w.moveActiveTab(1);
        break;
      case "pane.splitRight":
        w.split("row");
        break;
      case "pane.splitDown":
        w.split("column");
        break;
      case "pane.close":
        w.closePane();
        break;
      case "pane.focusLeft":
        w.focusNeighbor("left");
        return;
      case "pane.focusRight":
        w.focusNeighbor("right");
        return;
      case "pane.focusUp":
        w.focusNeighbor("up");
        return;
      case "pane.focusDown":
        w.focusNeighbor("down");
        return;
      case "terminal.copy":
        this.#copy();
        return;
      case "terminal.paste":
        this.#paste();
        return;
      case "terminal.clear":
        this.#activeController()?.terminal.clear();
        return;
      case "theme.cycle":
        this.setTheme(nextTheme(this.#state.theme));
        return;
    }
    this.#focusActivePane();
  }

  setTheme(theme: ThemePreference): void {
    applyTheme(this.#root, theme);
    saveTheme(this.#storage, theme);
    this.#set({ ...this.#state, theme });
    this.workspace.forEachTerminal((terminal) => terminal.refreshTheme());
  }

  dispose(): void {
    this.workspace.dispose();
    this.#listeners.clear();
  }

  #activeController(): TerminalController | undefined {
    const tab = this.workspace.activeTab;
    return tab ? this.workspace.terminal(tab.focused) : undefined;
  }

  #focusActivePane(): void {
    // Wait a frame: a split or new tab mounts its view first.
    requestAnimationFrame(() => this.#activeController()?.focus());
  }

  #copy(): void {
    const selection = this.#activeController()?.terminal.getSelection();
    if (selection) {
      this.#clipboard
        ?.writeText(selection)
        .catch((error: unknown) => console.warn("copy failed", error));
    }
  }

  #paste(): void {
    const controller = this.#activeController();
    this.#clipboard
      ?.readText()
      // `paste` applies bracketed paste mode, so a shell does not run pasted lines.
      .then((text) => controller?.terminal.paste(text))
      .catch((error: unknown) => console.warn("paste failed", error));
  }

  #set(state: ShellState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}

function readOverrides(storage: Storage | undefined): unknown {
  try {
    const raw = storage?.getItem(KEYMAP_STORAGE_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch (error) {
    console.warn("ignoring unreadable key bindings", error);
    return {};
  }
}
