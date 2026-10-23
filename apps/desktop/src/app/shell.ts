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
import { Broadcast, typedByUser } from "@/features/workspace/broadcast.ts";
import { type SavedSpec, SavedWorkspaces } from "@/features/workspace/saved.ts";
import { type PaneId, type Template, paneIds } from "@/features/workspace/layout.ts";
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
  SerialClient,
  SerialSpec,
  SnippetView,
  SnippetsClient,
  TextClipboard,
  VaultClient,
} from "@/ipc/index.ts";

import { type ThemePreference, applyTheme, loadTheme, nextTheme, saveTheme } from "./theme.ts";

const KEYMAP_STORAGE_KEY = "tauminus.keymap";
/** How long keys typed after a broadcast line wait for a prompt to show. */
const HOLD_MS = 500;
/** Enter, Ctrl+C, or Ctrl+D: the end of a secret typed at a prompt. */
// oxlint-disable-next-line no-control-regex -- these keys are controls
const ENDS_SECRET = /[\r\n\u0003\u0004]/u;
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
  "broadcast.toggle": "Broadcast input: add or remove this pane",
  "broadcast.tab": "Broadcast input to every pane in this tab",
  "broadcast.off": "Stop broadcasting input",
  "workspaces.open": "Workspaces: save or open a set of tabs",
  "serial.open": "Open a serial port",
};

interface ShellState {
  paletteOpen: boolean;
  connectOpen: boolean;
  vaultOpen: boolean;
  historyOpen: boolean;
  forwardsOpen: boolean;
  snippetsOpen: boolean;
  workspacesOpen: boolean;
  serialOpen: boolean;
  /**
   * A snippet waiting for its placeholders to be filled in, and the saved
   * hosts to run it on, if it runs on hosts rather than the focused pane.
   */
  snippetToFill: { snippet: SnippetView; hosts: readonly string[] | undefined } | undefined;
  /** A snippet waiting for the hosts to run it on. */
  snippetHosts: SnippetView | undefined;
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
  /** The panes that type together. */
  readonly broadcast = new Broadcast();
  /** Sets of tabs saved to open again. */
  readonly workspaces: SavedWorkspaces;
  /** Serial ports, when this window can reach them. */
  readonly serial: SerialClient | undefined;
  /** Settles once a host pane's session is up and its startup snippets typed. */
  readonly #ready = new WeakMap<
    TerminalController,
    { promise: Promise<boolean>; done: (ran: boolean) => void; settled: boolean }
  >();
  /** When broadcast last said it held a secret back, so it says so once per secret. */
  #heldBackAt = 0;
  /** Panes where the user is typing a secret, until Enter or Ctrl+C. */
  readonly #secret = new Set<PaneId>();
  /** Keys a source typed just after sending a line, waiting to be judged. */
  readonly #held = new Map<PaneId, { keys: string[]; timer: ReturnType<typeof setTimeout> }>();
  /** False from each start of a host pane's session until its startup snippets are typed. */
  readonly #startupTyped = new WeakMap<TerminalController, boolean>();
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
    serial?: SerialClient;
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
    this.workspaces = new SavedWorkspaces(options.storage);
    this.serial = options.serial;
    this.#state = {
      paletteOpen: false,
      connectOpen: false,
      vaultOpen: false,
      historyOpen: false,
      forwardsOpen: false,
      snippetsOpen: false,
      workspacesOpen: false,
      serialOpen: false,
      snippetToFill: undefined,
      snippetHosts: undefined,
      files: undefined,
      sidebarOpen: readFlag(options.storage, SIDEBAR_STORAGE_KEY, true),
      theme: loadTheme(options.storage),
    };
    applyTheme(options.root, this.#state.theme);

    this.workspace = new Workspace<TerminalController, SessionSpec>((paneId, spec) => {
      const links = options.links;
      const controller = new TerminalController(options.sessions, {
        ...(options.renderer ? { renderer: options.renderer } : {}),
        ...(spec ? { spec } : {}),
        ...(savedHostId(spec) ? { guard: this.hosts.guardFor(savedHostId(spec) ?? "") } : {}),
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
      controller.setInputListener((data) => this.#broadcastFrom(paneId, data));
      // A secret typed into a session that went away ends with it: the next
      // session starts at its own prompt.
      controller.subscribe(() => {
        if (controller.getStatus().kind !== "running") this.#forget(paneId);
      });
      // Not over Telnet: it reports running once TCP connects, and the text
      // would go to the device's login prompt.
      if (spec?.kind === "host") this.#startupOn(controller, spec.hostId);
      return controller;
    });
    // Panes of the shown tab are watched; the others collect activity.
    // O(P) per change, P ≤ 64.
    this.workspace.subscribe(() => {
      const { tabs, activeTabId } = this.workspace.getSnapshot();
      for (const tab of tabs) {
        for (const pane of paneIds(tab.root)) {
          this.workspace.terminal(pane)?.setWatched(tab.id === activeTabId);
        }
      }
    });
    // A closed pane leaves the broadcast. Read from the layout, which
    // changes before the pane is disposed. O(P), P ≤ 64.
    this.workspace.subscribe(() => {
      if (this.broadcast.getSnapshot().size === 0) return;
      const open = new Set(this.workspace.getSnapshot().tabs.flatMap((t) => paneIds(t.root)));
      for (const pane of this.broadcast.getSnapshot()) {
        if (!open.has(pane)) this.broadcast.remove(pane);
      }
      for (const pane of this.#secret) if (!open.has(pane)) this.#secret.delete(pane);
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

  /**
   * Runs a snippet in the focused pane, or on `hosts` when given, asking
   * first for its placeholders.
   */
  chooseSnippet(id: string, hosts?: readonly string[]): void {
    const snippet = this.snippets?.find(id);
    if (!snippet) return;
    const closed = { ...this.#state, snippetsOpen: false, snippetHosts: undefined };
    if (snippet.variables.length > 0) {
      this.#set({ ...closed, snippetToFill: { snippet, hosts } });
      return;
    }
    this.#set(closed);
    void this.runSnippet(id, {}, hosts);
  }

  /** Asks which saved hosts to run a snippet on. */
  chooseSnippetHosts(id: string): void {
    const snippet = this.snippets?.find(id);
    if (!snippet) return;
    this.#set({ ...this.#state, snippetsOpen: false, snippetHosts: snippet });
  }

  closeSnippetHosts(): void {
    if (this.#state.snippetHosts === undefined) return;
    this.#set({ ...this.#state, snippetHosts: undefined });
    this.#focusActivePane();
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
  async runSnippet(
    id: string,
    values: Record<string, string>,
    hosts?: readonly string[],
  ): Promise<void> {
    const snippets = this.snippets;
    const controller = this.#activeController();
    this.closeSnippetForm();
    if (!snippets) return;
    if (hosts) {
      await this.#runOnHosts(snippets, id, values, hosts);
      return;
    }
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
   * Opens `hosts` in a new tab, one pane each, broadcasting together, and
   * types the snippet into each once its session is up and its startup
   * snippets typed. A host that cannot connect is left out and named.
   */
  async #runOnHosts(
    snippets: SnippetsStore,
    id: string,
    values: Record<string, string>,
    hosts: readonly string[],
  ): Promise<void> {
    let text: string;
    try {
      text = await snippets.client.command(id, values);
    } catch (error) {
      this.hosts.notify(`Snippet: ${error instanceof Error ? error.message : String(error)}`);
      return;
    }
    const saved = this.hosts.getSnapshot().view.hosts;
    const specs: SessionSpec[] = [];
    const unable: string[] = [];
    for (const hostId of hosts) {
      const host = saved.find((h) => h.id === hostId);
      const spec = host ? specFor(host) : "deleted";
      if (typeof spec === "string") {
        unable.push(`${host?.label ?? hostId} (${spec})`);
        continue;
      }
      specs.push(spec);
    }
    const panes = this.#openGrid(specs);
    for (const spec of specs.slice(panes.length)) {
      if (spec.kind === "host" || spec.kind === "telnet") {
        unable.push(`${spec.label} (no room for more panes)`);
      }
    }
    // One notice: each would replace the one before.
    if (unable.length > 0) this.hosts.notify(`Not run on ${unable.join(", ")}`);
    this.broadcast.set(panes);
    for (const pane of panes) {
      const controller = this.workspace.terminal(pane);
      if (!controller) continue;
      void this.#ready.get(controller)?.promise.then((up) => {
        if (up) controller.type(text);
        else
          this.hosts.notify(`${controller.defaultTitle}: did not connect; the snippet did not run`);
      });
    }
    this.#focusActivePane();
  }

  /**
   * One new tab with a pane per spec, in a grid of about √n columns.
   * Stops at the pane limit. Returns the panes opened.
   */
  #openGrid(specs: readonly SessionSpec[]): PaneId[] {
    const [first, ...rest] = specs;
    if (!first || !this.workspace.newTab(first)) return [];
    try {
      return this.#fillGrid(specs.length, rest);
    } finally {
      // A grid opened at once has equal panes; halving each split would not.
      this.workspace.balance();
    }
  }

  /** The panes after the first, row then columns; see {@link #openGrid}. */
  #fillGrid(count: number, rest: readonly SessionSpec[]): PaneId[] {
    const focused = () => this.workspace.activeTab?.focused;
    const opened: PaneId[] = [];
    const head = focused();
    if (head) opened.push(head);
    const columns = Math.ceil(Math.sqrt(count));
    const rows = Math.ceil(count / columns);
    const heads = head ? [head] : [];
    let next = 0;
    // The first row, left to right.
    for (let c = 1; c < columns && next < rest.length; c += 1) {
      if (!this.workspace.split("row", rest[next])) return opened;
      next += 1;
      const pane = focused();
      if (pane) {
        opened.push(pane);
        heads.push(pane);
      }
    }
    // Then down each column.
    for (const top of heads) {
      this.workspace.focusPane(top);
      for (let r = 1; r < rows && next < rest.length; r += 1) {
        if (!this.workspace.split("column", rest[next])) return opened;
        next += 1;
        const pane = focused();
        if (pane) opened.push(pane);
      }
    }
    if (head) this.workspace.focusPane(head);
    return opened;
  }

  /**
   * Sends what the user typed in `from` to the other panes of its tab it
   * broadcasts with. For half a second after a line is sent, keys wait:
   * a password typed ahead of its prompt must be judged once the prompt
   * shows. Ctrl+C never waits, and drops what did.
   */
  #broadcastFrom(from: PaneId, data: string): void {
    if (!typedByUser(data)) return;
    const held = this.#held.get(from);
    if (data === "\u0003") {
      if (held) {
        clearTimeout(held.timer);
        this.#held.delete(from);
      }
    } else if (held) {
      held.keys.push(data);
      return;
    }
    this.#deliver(from, data);
  }

  /**
   * One key to every member in `from`'s tab: through each pane's prod
   * guard, Ctrl+C at once. A pane still connecting, or not done typing its
   * startup snippets, is skipped. A secret, once the user starts typing it
   * at a password prompt and until Enter or Ctrl+C, goes only to members
   * asking for one too; a prompt that echoes `*` stops matching after one
   * key, so the decision holds for the whole secret.
   */
  #deliver(from: PaneId, data: string): void {
    const tab = this.workspace.getSnapshot().tabs.find((t) => paneIds(t.root).includes(from));
    const inTab = new Set(tab ? paneIds(tab.root) : []);
    const targets = this.broadcast.targets(from).filter((pane) => inTab.has(pane));
    if (targets.length === 0) return;
    const interrupt = data === "\u0003";
    const ends = ENDS_SECRET.test(data);
    const source = this.workspace.terminal(from);
    const secret = !interrupt && (this.#secret.has(from) || source?.atSecretPrompt === true);
    for (const pane of targets) {
      const controller = this.workspace.terminal(pane);
      if (!controller) {
        this.broadcast.remove(pane);
      } else if (interrupt) {
        this.#secret.delete(pane);
        controller.interrupt();
      } else if (!this.#takesKeys(controller)) {
        if (ends) this.#secret.delete(pane);
        continue;
      } else if (secret && !(this.#secret.has(pane) || controller.atSecretPrompt)) {
        this.#secretHeldBack(controller);
      } else {
        if (secret && !ends) this.#secret.add(pane);
        if (ends) this.#secret.delete(pane);
        controller.typeShared(data);
      }
    }
    if (secret && !ends) this.#secret.add(from);
    if (ends || interrupt) this.#secret.delete(from);
    // A line went out: what comes next may be a password for a prompt not
    // drawn yet.
    if (!secret && /[\r\n]/u.test(data)) this.#hold(from);
  }

  /** Holds `from`'s keys for {@link HOLD_MS}, then sends them as the screen is then. */
  #hold(from: PaneId): void {
    const held = {
      keys: [] as string[],
      timer: setTimeout(() => {
        this.#held.delete(from);
        const [next, ...rest] = held.keys;
        if (next === undefined) return;
        this.#deliver(from, next);
        for (const key of rest) this.#broadcastFrom(from, key);
      }, HOLD_MS),
    };
    this.#held.set(from, held);
  }

  /**
   * A pane takes broadcast keys while its session runs, once that start's
   * startup snippets were typed: keys sent while it connects would wait and
   * arrive before them.
   */
  #takesKeys(controller: TerminalController): boolean {
    return (
      controller.getStatus().kind === "running" && this.#startupTyped.get(controller) !== false
    );
  }

  /** Drops `pane`'s secret latch and any keys held for it. */
  #forget(pane: PaneId): void {
    this.#secret.delete(pane);
    const held = this.#held.get(pane);
    if (held) {
      clearTimeout(held.timer);
      this.#held.delete(pane);
    }
  }

  /** Says once per secret which pane did not get it. */
  #secretHeldBack(controller: TerminalController): void {
    const now = Date.now();
    if (now - this.#heldBackAt < 2000) return;
    this.#heldBackAt = now;
    this.hosts.notify(
      `Broadcast held a secret back from ${controller.title || "a pane"}: it is not asking for one`,
    );
  }

  /**
   * Types a host's startup snippets each time the user brings its session
   * up: not after an automatic reconnect, and never into a tmux session.
   * A host with none costs no call.
   */
  #startupOn(controller: TerminalController, hostId: string): void {
    const { promise, done } = deferred<boolean>();
    const ready = { promise, done, settled: false };
    void promise.then(() => {
      ready.settled = true;
    });
    this.#ready.set(controller, ready);
    const snippets = this.snippets;
    let was = controller.getStatus().kind;
    controller.subscribe(() => {
      const now = controller.getStatus().kind;
      const started = now === "running" && was !== "running";
      // Back on its own after a loss: nobody is watching what the text
      // would land in (a `tmux attach` snippet's session, for one).
      const automatic = was === "reconnecting";
      was = now;
      // A session that ends before it ever came up never gets its keys:
      // a retry an hour later is not the moment the user ran them for.
      if (!ready.settled && (now === "failed" || now === "exited")) {
        done(false);
        return;
      }
      if (!started) return;
      const host = this.hosts.getSnapshot().view.hosts.find((h) => h.id === hostId);
      // A tmux session may already be running, from another pane or an
      // earlier run: the text would be typed into whatever it shows.
      if (!snippets || !host || host.startup.length === 0 || host.joinsTmux || automatic) {
        this.#startupTyped.set(controller, true);
        done(true);
        return;
      }
      this.#startupTyped.set(controller, false);
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
        )
        .finally(() => {
          this.#startupTyped.set(controller, true);
          done(true);
        });
    });
  }

  setSerialOpen(open: boolean): void {
    if (this.#state.serialOpen === open || (open && !this.serial)) return;
    this.#set({ ...this.#state, serialOpen: open });
    if (!open) this.#focusActivePane();
  }

  /** Opens a serial port in a new tab. */
  openSerial(port: SerialSpec): void {
    this.setSerialOpen(false);
    this.connect({ kind: "serial", port });
  }

  setWorkspacesOpen(open: boolean): void {
    if (this.#state.workspacesOpen === open) return;
    this.#set({ ...this.#state, workspacesOpen: open });
    if (!open) this.#focusActivePane();
  }

  /**
   * Saves every tab, its layout and what each pane opens, under `name`. A
   * typed-in connection through a proxy is kept as a local shell: a proxy
   * command read back from storage would run whatever was written there.
   * Returns what to tell the user, or undefined when it saved.
   */
  saveWorkspace(name: string): string | undefined {
    const dropped: string[] = [];
    const saved = (spec: SessionSpec | undefined): SavedSpec => {
      const hostId = savedHostId(spec);
      if (hostId) return { kind: "host", hostId };
      if (spec?.kind === "serial") return { kind: "serial", port: spec.port };
      if (spec?.kind === "ssh") {
        if (spec.target.route.kind === "direct") {
          return {
            kind: "ssh",
            destination: spec.target.destination,
            jumps: spec.target.jumps ?? [],
          };
        }
        dropped.push(formatEndpoint(spec.target.destination));
      }
      return { kind: "local" };
    };
    const tabs = this.workspace.templates().map((t) => mapTemplate(t, saved));
    if (!this.workspaces.save(name, tabs)) {
      return "A workspace needs a name and a tab, and at most 50 are kept";
    }
    return dropped.length > 0
      ? `Saved; ${dropped.join(", ")} went through a proxy and opens as a local shell`
      : undefined;
  }

  /**
   * Opens a saved workspace's tabs after the ones open now. A saved host
   * opens as it is saved now; one deleted since opens a local shell, and
   * the user is told.
   */
  async openWorkspace(id: string): Promise<void> {
    const workspace = this.workspaces.find(id);
    if (!workspace) return;
    // An empty list while it loads would read every saved host as deleted,
    // and open local shells where the user expects remote ones.
    if (this.hosts.getSnapshot().status === "loading") await this.hosts.load();
    if (this.hosts.getSnapshot().status !== "ready") {
      this.hosts.notify(`${workspace.name}: not opened; the saved hosts could not be read`);
      return;
    }
    const hosts = this.hosts.getSnapshot().view.hosts;
    const missing = new Set<string>();
    const open = (spec: SavedSpec): SessionSpec => {
      if (spec.kind === "serial") return { kind: "serial", port: spec.port };
      if (spec.kind === "ssh") {
        return {
          kind: "ssh",
          target: {
            destination: spec.destination,
            jumps: [...spec.jumps],
            route: { kind: "direct" },
          },
        };
      }
      if (spec.kind === "host") {
        const host = hosts.find((h) => h.id === spec.hostId);
        const opened = host ? specFor(host) : undefined;
        if (opened && typeof opened !== "string") return opened;
        missing.add(host?.label ?? "a deleted host");
      }
      return { kind: "local" };
    };
    let opened = 0;
    for (const tab of workspace.tabs) {
      if (this.workspace.openTemplate(mapTemplate(tab, open))) opened += 1;
    }
    this.setWorkspacesOpen(false);
    const notes = [
      ...(missing.size > 0 ? [`${[...missing].join(", ")} opened as a local shell`] : []),
      ...(opened < workspace.tabs.length
        ? [`${workspace.tabs.length - opened} tabs did not fit under the pane limit`]
        : []),
    ];
    if (notes.length > 0) this.hosts.notify(`${workspace.name}: ${notes.join("; ")}`);
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
    const spec = specFor(host);
    if (typeof spec === "string") {
      this.hosts.notify(`${host.label}: ${spec}`);
      return;
    }
    this.connect(spec);
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
      case "broadcast.toggle": {
        const pane = this.workspace.activeTab?.focused;
        if (pane) this.broadcast.toggle(pane);
        break;
      }
      case "broadcast.tab": {
        const tab = this.workspace.activeTab;
        if (tab) this.broadcast.set(paneIds(tab.root));
        break;
      }
      case "workspaces.open":
        this.setWorkspacesOpen(true);
        return;
      case "serial.open":
        this.setSerialOpen(true);
        return;
      case "broadcast.off":
        this.broadcast.clear();
        break;
      case "files.open": {
        const spec = this.workspace.focusedSpec();
        const host =
          spec?.kind === "host"
            ? this.hosts.getSnapshot().view.hosts.find((h) => h.id === spec.hostId)
            : undefined;
        if (host) this.openFiles(host);
        else if (spec?.kind === "telnet") this.hosts.notify("Telnet carries no files");
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
    const host = savedHostId(spec) ?? null;
    const label =
      spec?.kind === "host" || spec?.kind === "telnet"
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

/** What opens a saved host, by its protocol, or why it cannot open. */
function specFor(host: HostView): SessionSpec | string {
  if (host.protocol === "telnet") {
    if (host.problem) return host.problem;
    return { kind: "telnet", hostId: host.id, label: host.label };
  }
  if (!host.target) return host.problem ?? "cannot connect";
  return { kind: "host", hostId: host.id, label: host.label, target: host.target };
}

/** The saved host a pane opened, whatever its protocol. */
function savedHostId(spec: SessionSpec | undefined): string | undefined {
  return spec?.kind === "host" || spec?.kind === "telnet" ? spec.hostId : undefined;
}

/** A template with each pane's spec turned into another. O(n). */
function mapTemplate<A, B>(template: Template<A>, map: (spec: A) => B): Template<B> {
  return template.kind === "pane"
    ? { kind: "pane", spec: map(template.spec) }
    : { ...template, children: template.children.map((child) => mapTemplate(child, map)) };
}

/** A promise and what settles it. */
function deferred<T>(): { promise: Promise<T>; done: (value: T) => void } {
  let done: ((value: T) => void) | undefined;
  const promise = new Promise<T>((resolve) => {
    done = resolve;
  });
  // The executor runs at once, so `done` is set here.
  return { promise, done: done ?? noop };
}

function noop(): void {}

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
