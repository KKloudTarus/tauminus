// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import type { WebglAddon } from "@xterm/addon-webgl";
import { type ITheme, Terminal } from "@xterm/xterm";

import { formatEndpoint } from "@/features/ssh/destination.ts";
import {
  type AnswerSpec,
  type ErrorCode,
  type GridSize,
  type PromptKind,
  type SessionClient,
  SessionCommandError,
  type SessionSpec,
  type TerminalSession,
} from "@/ipc/index.ts";

import { ACK_THRESHOLD_BYTES, AckBatcher } from "./ack-batcher.ts";
import { InputQueue, binaryStringToBytes } from "./input-queue.ts";
import { type RendererPreference, chooseRenderer } from "./renderer.ts";
import { webglBudget } from "./webgl-budget.ts";

/** Delay before retrying an acknowledgement whose IPC call failed. */
const ACK_RETRY_MS = 100;

const FONT_FAMILY =
  '"JetBrains Mono", "Cascadia Code", "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace';

export type TerminalStatus =
  | { kind: "starting" }
  | { kind: "running" }
  | { kind: "exited"; code: number | null }
  | { kind: "failed"; message: string; code: ErrorCode | undefined };

const CANCEL: AnswerSpec = { kind: "cancel" };
const LOCAL: SessionSpec = { kind: "local" };

type Listener = () => void;

/**
 * One terminal and the session behind it, independent of React.
 *
 * Views attach and detach; they never open or close the session. A pane can
 * move between tabs or splits, which remounts its view, without killing the
 * shell or losing the screen. The session opens on the first attach, once the
 * grid size is known, and closes only on `dispose`.
 *
 * Output bytes go from the session channel straight into xterm.js. Only the
 * coarse status is observable state.
 */
export class TerminalController {
  readonly terminal: Terminal;
  readonly spec: SessionSpec;
  readonly #sessions: SessionClient;
  readonly #fit = new FitAddon();
  readonly #search = new SearchAddon();
  readonly #acks = new AckBatcher(ACK_THRESHOLD_BYTES);
  readonly #input: InputQueue;
  readonly #listeners = new Set<Listener>();
  #status: TerminalStatus = { kind: "starting" };
  #title = "";
  #session: TerminalSession | undefined;
  #opening = false;
  /**
   * Set by the first attach. A view that remounts after the session ended
   * must show the ended session, not quietly start a new one; only `retry`
   * opens again.
   */
  #started = false;
  /** Questions from the connect, oldest first; the view shows the first. */
  #prompts: { prompt: PromptKind; resolve: (answer: AnswerSpec) => void }[] = [];
  #disposed = false;
  /**
   * Parsed bytes not yet acknowledged to the backend: waiting for `open` to
   * resolve, for the ack in flight to finish, or for a retry after a failure.
   * Never dropped, because a lost acknowledgement can leave the session
   * paused for good.
   */
  #unacknowledged = 0;
  #ackInFlight = false;
  #ackRetry: ReturnType<typeof setTimeout> | undefined;
  #host: HTMLElement | undefined;
  #webgl: WebglAddon | undefined;
  /**
   * Set once this terminal gives up WebGL, after a context loss or an
   * eviction from the budget. It stays on the DOM renderer from then on:
   * loading a second WebGL addon into the same terminal renders nothing on
   * WebKitGTK.
   */
  #webglRetired = false;
  #webglLoading = false;
  #observer: ResizeObserver | undefined;
  #frame = 0;
  /**
   * The grid size the session last heard. Compared with the terminal's own
   * size, not with the host's: a remount refits the terminal before any
   * observer runs, and the session must still be told.
   */
  #sentSize: GridSize | undefined;

  constructor(
    sessions: SessionClient,
    options: { renderer?: RendererPreference; spec?: SessionSpec } = {},
  ) {
    this.#sessions = sessions;
    this.spec = options.spec ?? LOCAL;
    this.#webglRetired =
      chooseRenderer(options.renderer ?? "auto", globalThis.navigator?.userAgent ?? "") === "dom";
    this.terminal = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: FONT_FAMILY,
      fontSize: 13,
      scrollback: 10_000,
    });
    this.terminal.loadAddon(this.#fit);
    this.terminal.loadAddon(this.#search);
    this.terminal.loadAddon(new Unicode11Addon());
    this.terminal.unicode.activeVersion = "11";

    this.#input = new InputQueue(
      {
        writeText: (data) => this.#session?.write(data) ?? Promise.resolve(),
        writeBytes: (data) => this.#session?.writeBytes(data) ?? Promise.resolve(),
      },
      ignoreEndedSession,
    );
    this.terminal.onTitleChange((title) => {
      // Remote programs set the title; keep it short and printable.
      this.#title = sanitizeTitle(title);
      this.#notify();
    });
    this.terminal.onData((data) => this.#input.push({ kind: "text", data }));
    this.terminal.onBinary((data) =>
      this.#input.push({ kind: "bytes", data: binaryStringToBytes(data) }),
    );
  }

  get status(): TerminalStatus {
    return this.#status;
  }

  get search(): SearchAddon {
    return this.#search;
  }

  get rendererKind(): "webgl" | "dom" {
    return this.#webgl ? "webgl" : "dom";
  }

  /** For `useSyncExternalStore`. */
  subscribe = (listener: Listener): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getStatus = (): TerminalStatus => this.#status;

  /** The question waiting for the user, if any. For `useSyncExternalStore`. */
  getPrompt = (): PromptKind | undefined => this.#prompts[0]?.prompt;

  /** Answers the question `getPrompt` returned. */
  answer(answer: AnswerSpec): void {
    const pending = this.#prompts.shift();
    if (!pending) return;
    pending.resolve(answer);
    this.#notify();
    if (this.#prompts.length === 0) this.focus();
  }

  /** Opens the session again after it failed to start. */
  retry(): void {
    if (this.#disposed || this.#status.kind !== "failed" || this.#opening) return;
    this.terminal.reset();
    this.#setStatus({ kind: "starting" });
    this.#open();
  }

  /** What to call this terminal before a program sets a title. */
  get defaultTitle(): string {
    switch (this.spec.kind) {
      case "host":
        return this.spec.label;
      case "ssh":
        return formatEndpoint(this.spec.target.destination);
      default:
        return "Terminal";
    }
  }

  /** The title the shell or program set with OSC 0 or 2, or "" if none. */
  get title(): string {
    return this.#title;
  }

  /**
   * Lets the app handle its own shortcuts before xterm.js sees the key.
   * `handle` returns true when it consumed the event.
   */
  setKeyHandler(handle: (event: KeyboardEvent) => boolean): void {
    this.terminal.attachCustomKeyEventHandler(
      (event) => !(event.type === "keydown" && handle(event)),
    );
  }

  /** Re-reads the theme tokens, after the app switches light or dark. */
  refreshTheme(): void {
    if (this.#host) {
      this.terminal.options.theme = themeFrom(this.#host);
    }
  }

  /** Shows the terminal in `host`. Opens the session on the first call. */
  attach(host: HTMLElement): void {
    if (this.#disposed) {
      return;
    }
    if (this.#host && this.#host !== host) {
      this.detach();
    }
    this.#host = host;
    this.terminal.options.theme = themeFrom(host);
    if (this.terminal.element) {
      host.append(this.terminal.element);
    } else {
      this.terminal.open(host);
    }
    this.#acquireWebgl();
    this.#fit.fit();
    this.#resizeSession();
    this.#observer = new ResizeObserver(() => this.#scheduleFit());
    this.#observer.observe(host);
    if (!this.#started) {
      this.#started = true;
      this.#open();
    }
  }

  /** Removes the terminal from its host. The session keeps running. */
  detach(): void {
    this.#observer?.disconnect();
    this.#observer = undefined;
    cancelAnimationFrame(this.#frame);
    this.terminal.element?.remove();
    this.#host = undefined;
  }

  /** Focuses the terminal, unless a prompt is open: the prompt keeps focus. */
  focus(): void {
    if (this.#prompts.length > 0) return;
    this.terminal.focus();
  }

  /** Closes the session and frees the terminal. The controller is unusable after. */
  dispose(): void {
    if (this.#disposed) {
      return;
    }
    this.#disposed = true;
    this.detach();
    clearTimeout(this.#ackRetry);
    this.#retireWebgl();
    this.#input.close();
    for (const pending of this.#prompts.splice(0)) pending.resolve(CANCEL);
    this.#session?.close().catch(ignoreEndedSession);
    this.#session = undefined;
    this.terminal.dispose();
    this.#listeners.clear();
  }

  #open(): void {
    this.#opening = true;
    this.#sentSize = { cols: this.terminal.cols, rows: this.terminal.rows };
    this.#sessions
      .open(this.spec, this.#sentSize, {
        onPrompt: (prompt) => this.#ask(prompt),
        onOutput: (data) => this.#write(data),
        onExit: (code) => {
          this.#session = undefined;
          this.#setStatus({ kind: "exited", code });
        },
      })
      .then((opened) => {
        this.#opening = false;
        if (this.#disposed) {
          opened.close().catch(ignoreEndedSession);
          return;
        }
        this.#session = opened;
        if (this.#status.kind === "starting") {
          this.#setStatus({ kind: "running" });
        }
        this.#flushAcknowledgements();
        // The grid may have changed while the session was opening.
        this.#resizeSession();
      })
      .catch((error: unknown) => {
        this.#opening = false;
        // Questions the connect left unanswered no longer matter.
        for (const pending of this.#prompts.splice(0)) pending.resolve(CANCEL);
        if (!this.#disposed) {
          const code = error instanceof SessionCommandError ? error.code : undefined;
          this.#setStatus({ kind: "failed", message: errorMessage(error), code });
        }
      });
  }

  #ask(prompt: PromptKind): Promise<AnswerSpec> {
    if (this.#disposed) return Promise.resolve(CANCEL);
    return new Promise((resolve) => {
      this.#prompts.push({ prompt, resolve });
      this.#notify();
    });
  }

  #write(data: Uint8Array): void {
    if (this.#disposed) {
      return;
    }
    this.terminal.write(data, () => {
      const ready = this.#acks.add(data.byteLength);
      if (ready > 0) {
        this.#acknowledge(ready);
      }
    });
  }

  #acknowledge(bytes: number): void {
    this.#unacknowledged += bytes;
    this.#flushAcknowledgements();
  }

  /** Sends everything unacknowledged, with at most one ack call in flight. */
  #flushAcknowledgements(): void {
    const session = this.#session;
    if (!session || this.#ackInFlight || this.#unacknowledged === 0 || this.#disposed) {
      return;
    }
    const bytes = this.#unacknowledged;
    this.#unacknowledged = 0;
    this.#ackInFlight = true;
    session.acknowledge(bytes).then(
      () => {
        this.#ackInFlight = false;
        this.#flushAcknowledgements();
      },
      (error: unknown) => {
        this.#ackInFlight = false;
        if (isEndedSession(error)) {
          return;
        }
        console.error("acknowledgement failed; retrying", error);
        this.#unacknowledged += bytes;
        clearTimeout(this.#ackRetry);
        this.#ackRetry = setTimeout(() => this.#flushAcknowledgements(), ACK_RETRY_MS);
      },
    );
  }

  /** Takes a WebGL slot from the shared budget, loading the addon on first use. */
  #acquireWebgl(): void {
    if (this.#webglRetired) {
      return;
    }
    if (this.#webgl) {
      webglBudget.acquire(this, () => this.#retireWebgl());
      return;
    }
    if (this.#webglLoading) {
      return;
    }
    // Loaded on demand: webviews that use the DOM renderer never fetch or
    // parse the addon.
    this.#webglLoading = true;
    import("@xterm/addon-webgl")
      .then(({ WebglAddon }) => {
        this.#webglLoading = false;
        if (this.#disposed || this.#webglRetired || this.#webgl) {
          return;
        }
        this.#webgl = this.#loadWebgl(WebglAddon);
        if (this.#webgl) {
          webglBudget.acquire(this, () => this.#retireWebgl());
        }
      })
      .catch((error: unknown) => {
        this.#webglLoading = false;
        this.#webglRetired = true;
        console.warn("WebGL renderer unavailable; using the DOM renderer", error);
      });
  }

  #loadWebgl(Addon: typeof WebglAddon): WebglAddon | undefined {
    try {
      const webgl = new Addon();
      webgl.onContextLoss(() => this.#retireWebgl());
      this.terminal.loadAddon(webgl);
      return webgl;
    } catch {
      // No WebGL2 context: xterm.js keeps the DOM renderer.
      this.#webglRetired = true;
      return undefined;
    }
  }

  /** Moves this terminal to the DOM renderer for good. Idempotent. */
  #retireWebgl(): void {
    webglBudget.release(this);
    this.#webglRetired = true;
    const webgl = this.#webgl;
    this.#webgl = undefined;
    webgl?.dispose();
  }

  /** Resize events arrive in bursts while a window is dragged; apply one per frame. */
  #scheduleFit(): void {
    cancelAnimationFrame(this.#frame);
    this.#frame = requestAnimationFrame(() => {
      const proposed = this.#fit.proposeDimensions();
      if (!proposed) return;
      if (proposed.cols !== this.terminal.cols || proposed.rows !== this.terminal.rows) {
        this.#fit.fit();
      }
      this.#resizeSession();
    });
  }

  /** Tells the session the grid size, once per change. */
  #resizeSession(): void {
    const session = this.#session;
    const { cols, rows } = this.terminal;
    if (!session || (this.#sentSize?.cols === cols && this.#sentSize.rows === rows)) return;
    this.#sentSize = { cols, rows };
    session.resize({ cols, rows }).catch(ignoreEndedSession);
  }

  #setStatus(status: TerminalStatus): void {
    this.#status = status;
    this.#notify();
  }

  #notify(): void {
    for (const listener of this.#listeners) {
      listener();
    }
  }
}

function themeFrom(element: HTMLElement): ITheme {
  const style = getComputedStyle(element);
  const read = (name: string) => style.getPropertyValue(name).trim() || undefined;
  const theme: ITheme = {};
  const background = read("--terminal-background");
  const foreground = read("--terminal-foreground");
  const cursor = read("--terminal-cursor");
  const selection = read("--terminal-selection");
  if (background) theme.background = background;
  if (foreground) theme.foreground = foreground;
  if (cursor) theme.cursor = cursor;
  if (selection) theme.selectionBackground = selection;
  return theme;
}

/**
 * Writes and acknowledgements race with session exit, so `not_found` is
 * expected and ignored. Anything else is a bug worth seeing in the console.
 */
function ignoreEndedSession(error: unknown): void {
  if (isEndedSession(error)) {
    return;
  }
  console.error("terminal session command failed", error);
}

function isEndedSession(error: unknown): boolean {
  return error instanceof SessionCommandError && error.code === "not_found";
}

/** Strips C0 and C1 control characters and caps the length; titles come from untrusted output. O(n). */
export function sanitizeTitle(title: string): string {
  let clean = "";
  for (const char of title) {
    const code = char.codePointAt(0) ?? 0;
    if (code >= 0x20 && !(code >= 0x7f && code <= 0x9f)) clean += char;
  }
  return clean.trim().slice(0, 120);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
