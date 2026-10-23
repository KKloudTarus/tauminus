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
import type { RecordedCommand } from "@/features/terminal/recorder.ts";
import { formatEndpoint } from "@/features/ssh/destination.ts";
import type { RendererPreference } from "@/features/terminal/renderer.ts";
import { Workspace } from "@/features/workspace/workspace.ts";
import { HostsStore } from "@/features/hosts/hosts-store.ts";
import { VaultStore } from "@/features/vault/vault-store.ts";
import { ForwardsStore } from "@/features/forwards/forwards-store.ts";
import { FilesStore } from "@/features/files/files-store.ts";
import { SnippetsStore } from "@/features/snippets/snippets-store.ts";
import type {
  FilesClient,
  ForwardsClient,
  HistoryClient,
  HostView,
  HostsClient,
  LinkOpener,
  SessionClient,
  SessionSpec,
  SnippetView,
  SnippetsClient,
  TextClipboard,
  VaultClient,
} from "@/ipc/index.ts";

import { type ThemePreference, applyTheme, loadTheme, nextTheme, saveTheme } from "./theme.ts";

const KEYMAP_STORAGE_KEY = "tauminus.keymap";
const SIDEBAR_STORAGE_KEY = "tauminus.sidebar";

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
  "hosts.toggle": "Show or hide saved hosts",
  "hosts.import": "Import hosts from ~/.ssh/config",
  "vault.open": "Open the vault",
  "vault.lock": "Lock the vault",
  "history.open": "Search the command history",
  "forwards.open": "Port forwarding",
  "files.open": "Files on this host (SFTP)",
  "snippets.open": "Snippets",
};

interface ShellState {
  paletteOpen: boolean;
  connectOpen: boolean;
  vaultOpen: boolean;
  historyOpen: boolean;
  forwardsOpen: boolean;
  snippetsOpen: boolean;
  /** A snippet waiting for its placeholders to be filled in, if any. */
  snippetToFill: SnippetView | undefined;
  /** The host whose files are open, if any. */
  files: { host: HostView; store: FilesStore } | undefined;
  sidebarOpen: boolean;
  theme: ThemePreference;
}

/**
 * The app outside React: the workspace, the keymap, and UI state that
 * shortcuts change. One `run` dispatches every action, whether it came from a
 * key, the palette, or a button.
 */
export class AppShell {
  readonly workspace: Workspace<TerminalController, SessionSpec>;
  readonly hosts: HostsStore;
  readonly vault: VaultStore;
  /** Files on hosts, when this window has a backend for them. */
  readonly files: FilesClient | undefined;
  /** Port forwarding, when this window has a backend for it. */
  readonly forwards: ForwardsStore | undefined;
  /** Saved snippets, when this window has a backend for them. */
  readonly snippets: SnippetsStore | undefined;
  /** A connect waiting for the vault to unlock. */
  #afterUnlock: (() => void) | undefined;
  readonly keymap: Keymap;
  readonly platform: Platform;
  readonly #listeners = new Set<() => void>();
  #state: ShellState;

  constructor(options: {
    sessions: SessionClient;
    hosts: HostsClient;
    vault: VaultClient;
    links?: LinkOpener;
    userAgent: string;
    storage: Storage | undefined;
    root: HTMLElement;
    clipboard: TextClipboard | undefined;
    /** Where commands are recorded; none keeps no history. */
    history?: HistoryClient;
    forwards?: ForwardsClient;
    files?: FilesClient;
    snippets?: SnippetsClient;
    renderer?: RendererPreference;
  }) {
    this.platform = detectPlatform(options.userAgent);
    this.#storage = options.storage;
    this.#root = options.root;
    this.#clipboard = options.clipboard;
    this.#history = options.history;
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
    this.hosts = new HostsStore(options.hosts);
    this.vault = new VaultStore(options.vault, undefined, options.storage);
    this.files = options.files;
    this.forwards = options.forwards
      ? new ForwardsStore(options.forwards, (retry) => this.unlockVaultThen(retry))
      : undefined;
    this.snippets = options.snippets ? new SnippetsStore(options.snippets) : undefined;
    this.#state = {
      paletteOpen: false,
      connectOpen: false,
      vaultOpen: false,
      historyOpen: false,
      forwardsOpen: false,
      snippetsOpen: false,
      snippetToFill: undefined,
      files: undefined,
      sidebarOpen: readFlag(options.storage, SIDEBAR_STORAGE_KEY, true),
      theme: loadTheme(options.storage),
    };
    applyTheme(options.root, this.#state.theme);

    this.workspace = new Workspace<TerminalController, SessionSpec>((_paneId, spec) => {
      const links = options.links;
      const controller = new TerminalController(options.sessions, {
        ...(options.renderer ? { renderer: options.renderer } : {}),
        ...(spec ? { spec } : {}),
        ...(spec?.kind === "host" ? { guard: this.hosts.guardFor(spec.hostId) } : {}),
        ...(this.#history ? { record: this.#recorderFor(spec) } : {}),
        mac: this.platform === "mac",
        ...(links
          ? {
              openLink: (url: string) => {
                links
                  .open(url)
                  .catch((error: unknown) => console.warn("could not open link", error));
              },
            }
          : {}),
      });
      controller.setKeyHandler((event) => this.handleKey(event));
      if (spec?.kind === "host") this.#startupOn(controller, spec.hostId);
      return controller;
    });
  }

  readonly #storage: Storage | undefined;
  readonly #root: HTMLElement;
  readonly #clipboard: TextClipboard | undefined;
  readonly #history: HistoryClient | undefined;

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

  setHistoryOpen(open: boolean): void {
    if (this.#state.historyOpen === open) return;
    this.#set({ ...this.#state, historyOpen: open });
    if (!open) this.#focusActivePane();
  }

  /** Opens the files of a saved host, closing any others first. */
  openFiles(host: HostView): void {
    if (!this.files) return;
    this.#state.files?.store.close();
    const store = new FilesStore(this.files, host.id, (retry) => this.unlockVaultThen(retry));
    this.#set({ ...this.#state, files: { host, store } });
    void store.open();
  }

  closeFiles(): void {
    this.#state.files?.store.close();
    this.#set({ ...this.#state, files: undefined });
    this.#focusActivePane();
  }

  setForwardsOpen(open: boolean): void {
    if (this.#state.forwardsOpen === open) return;
    this.#set({ ...this.#state, forwardsOpen: open });
    if (!open) this.#focusActivePane();
  }

  /** Starts or stops a saved rule, with the forwarding list open to show it. */
  toggleForward(hostId: string, ruleId: string): void {
    if (!this.forwards) return;
    this.setForwardsOpen(true);
    void this.forwards.toggle(hostId, ruleId);
  }

  setSnippetsOpen(open: boolean): void {
    if (this.#state.snippetsOpen === open) return;
    // Loaded at start and kept current by each change; a failed load retries.
    if (open && !this.snippets?.getSnapshot().loaded) void this.snippets?.load();
    this.#set({ ...this.#state, snippetsOpen: open });
    if (!open) this.#focusActivePane();
  }

  /** Runs a snippet in the focused pane, asking first for its placeholders. */
  chooseSnippet(id: string): void {
    const snippet = this.snippets?.find(id);
    if (!snippet) return;
    if (snippet.variables.length > 0) {
      this.#set({ ...this.#state, snippetsOpen: false, snippetToFill: snippet });
      return;
    }
    this.#set({ ...this.#state, snippetsOpen: false });
    void this.runSnippet(id, {});
  }

  closeSnippetForm(): void {
    if (this.#state.snippetToFill === undefined) return;
    this.#set({ ...this.#state, snippetToFill: undefined });
    this.#focusActivePane();
  }

  /**
   * Types a snippet into the focused pane as keys, through the prod guard
   * on a guarded host, which asks before it runs.
   */
  async runSnippet(id: string, values: Record<string, string>): Promise<void> {
    const snippets = this.snippets;
    const controller = this.#activeController();
    this.closeSnippetForm();
    if (!snippets) return;
    if (!controller) {
      this.hosts.notify("Open a terminal to run a snippet in");
      return;
    }
    try {
      controller.type(await snippets.client.command(id, values));
    } catch (error) {
      this.hosts.notify(`Snippet: ${error instanceof Error ? error.message : String(error)}`);
    }
    this.#focusActivePane();
  }

  /**
   * Types a host's startup snippets each time its session comes up,
   * including after a reconnect. A host with none costs no call.
   */
  #startupOn(controller: TerminalController, hostId: string): void {
    const snippets = this.snippets;
    if (!snippets) return;
    let was = controller.getStatus().kind;
    controller.subscribe(() => {
      const now = controller.getStatus().kind;
      const started = now === "running" && was !== "running";
      was = now;
      if (!started) return;
      const host = this.hosts.getSnapshot().view.hosts.find((h) => h.id === hostId);
      if (!host || host.startup.length === 0) return;
      snippets.client
        .startup(hostId)
        .then(({ text, skipped }) => {
          if (text) controller.type(text);
          for (const s of skipped) {
            const name = snippets.find(s.id)?.name ?? s.id;
            this.hosts.notify(`${host.label}: startup snippet ${name} skipped: ${s.reason}`);
          }
        })
        .catch((error: unknown) =>
          this.hosts.notify(
            `${host.label}: startup snippets did not run: ${error instanceof Error ? error.message : String(error)}`,
          ),
        );
    });
  }

  /** The history client, when this window keeps one. */
  get history(): HistoryClient | undefined {
    return this.#history;
  }

  /** Types `text` into the focused terminal without running it. */
  insertText(text: string): void {
    // Bracketed paste: a shell takes it as text, not as keys to run.
    this.#activeController()?.terminal.paste(text);
    this.#focusActivePane();
  }

  setVaultOpen(open: boolean): void {
    if (this.#state.vaultOpen === open) return;
    if (!open) this.#afterUnlock = undefined;
    this.#set({ ...this.#state, vaultOpen: open });
    if (!open) this.#focusActivePane();
  }

  /** Opens the vault, then runs `retry` once it is unlocked. Retries at
   * once when the vault is already open, for example from another pane. */
  unlockVaultThen(retry: () => void): void {
    void this.vault.retryOrAsk(retry, () => {
      this.#afterUnlock = retry;
      this.setVaultOpen(true);
    });
  }

  /** The vault dialog unlocked the vault: run what waited for it. */
  vaultUnlocked(): void {
    const retry = this.#afterUnlock;
    if (!retry) return;
    this.setVaultOpen(false);
    retry();
  }

  /** Connects to a saved host in a new tab, or says why it cannot. */
  connectHost(host: HostView): void {
    if (!host.target) {
      this.hosts.notify(`${host.label}: ${host.problem ?? "cannot connect"}`);
      return;
    }
    this.connect({ kind: "host", hostId: host.id, label: host.label, target: host.target });
  }

  setSidebarOpen(open: boolean): void {
    if (this.#state.sidebarOpen === open) return;
    writeFlag(this.#storage, SIDEBAR_STORAGE_KEY, open);
    this.#set({ ...this.#state, sidebarOpen: open });
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
      case "hosts.toggle":
        this.setSidebarOpen(!this.#state.sidebarOpen);
        return;
      case "vault.open":
        this.setVaultOpen(true);
        return;
      case "history.open":
        this.setHistoryOpen(true);
        return;
      case "forwards.open":
        this.setForwardsOpen(true);
        return;
      case "snippets.open":
        this.setSnippetsOpen(true);
        return;
      case "files.open": {
        const spec = this.workspace.focusedSpec();
        const host =
          spec?.kind === "host"
            ? this.hosts.getSnapshot().view.hosts.find((h) => h.id === spec.hostId)
            : undefined;
        if (host) this.openFiles(host);
        else this.hosts.notify("Open files from a saved host: its ▤ button in the sidebar");
        return;
      }
      case "vault.lock":
        this.vault.lock().catch((error: unknown) => console.warn("lock failed", error));
        return;
      case "hosts.import":
        this.setSidebarOpen(true);
        this.hosts
          .importConfig()
          .catch((error: unknown) => this.hosts.notify(`Import failed: ${String(error)}`));
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

  /** Where one terminal's commands go: the saved host's id, or none. */
  #recorderFor(spec: SessionSpec | undefined): (entry: RecordedCommand) => void {
    const host = spec?.kind === "host" ? spec.hostId : null;
    const label =
      spec?.kind === "host"
        ? spec.label
        : spec?.kind === "ssh"
          ? formatEndpoint(spec.target.destination)
          : "Local";
    return (entry) => {
      this.#history
        ?.record({
          host,
          label,
          command: entry.command,
          output: entry.output,
          exit: entry.exit,
          started: Math.floor(entry.startedMs / 1000),
        })
        .catch((error: unknown) => console.warn("history record failed", error));
    };
  }

  /** Puts `text` on the system clipboard. */
  copyText(text: string): void {
    this.#clipboard?.writeText(text).catch((error: unknown) => console.warn("copy failed", error));
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

function readFlag(storage: Storage | undefined, key: string, fallback: boolean): boolean {
  try {
    const value = storage?.getItem(key);
    return value === null || value === undefined ? fallback : value === "1";
  } catch {
    return fallback;
  }
}

function writeFlag(storage: Storage | undefined, key: string, value: boolean): void {
  try {
    storage?.setItem(key, value ? "1" : "0");
  } catch {
    // Remembering the sidebar is a convenience; private mode may refuse it.
  }
}
