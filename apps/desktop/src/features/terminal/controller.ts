// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
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
import { CommandGuard, type GuardRequest, type GuardSource } from "./command-guard.ts";
import { startUnannouncedCompositions } from "./ime.ts";
import { InputQueue, binaryStringToBytes } from "./input-queue.ts";
import { asksToOpen, isOpenableLink } from "./links.ts";
import { type Lane, type OutputScheduler, outputScheduler } from "./output-scheduler.ts";
import { defusePaste } from "./paste.ts";
import { type RendererPreference, chooseRenderer } from "./renderer.ts";
import { dropsRepeat } from "./repeat-guard.ts";
import { TextareaPin } from "./textarea-pin.ts";
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
  /** Output on its way to xterm.js, parsed in turns shared with other panes. */
  readonly #output: Lane;
  readonly #input: InputQueue;
  readonly #listeners = new Set<Listener>();
  readonly #openLink: ((url: string) => void) | undefined;
  readonly #mac: boolean;
  #status: TerminalStatus = { kind: "starting" };
  #title = "";
  #session: TerminalSession | undefined;
  #opening = false;
  /**
   * Settles when the current open does: with the session, or undefined if
   * it failed or the pane went away. Input waits on it instead of being
   * dropped, because the shell may need an answer before `open` returns:
   * Windows' ConPTY asks for the cursor position at startup and holds the
   * shell until xterm.js replies.
   */
  #opened: Promise<TerminalSession | undefined> = Promise.resolve(undefined);
  #settleOpened: ((session: TerminalSession | undefined) => void) | undefined;
  /**
   * Set by the first attach. A view that remounts after the session ended
   * must show the ended session, not quietly start a new one; only `retry`
   * opens again.
   */
  #started = false;
  /** Checks commands before they run on a guarded host. */
  readonly #guard: CommandGuard | undefined;
  readonly #guardSource: GuardSource | undefined;
  /** Output parses so far, and who waits for the next; for the guard's echo wait. */
  #parses = 0;
  readonly #parseWaiters = new Set<() => void>();
  /** The guard's question, while the user decides. */
  #guardRequest: { request: GuardRequest; resolve: (run: boolean) => void } | undefined;
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
  /** Removes the IME fix; set once the terminal has its textarea. */
  #stopImeFix: (() => void) | undefined;
  /** Holds the textarea still during bulk output; set with the textarea. */
  #pin: TextareaPin | undefined;
  #frame = 0;
  /**
   * The grid size the session last heard. Compared with the terminal's own
   * size, not with the host's: a remount refits the terminal before any
   * observer runs, and the session must still be told.
   */
  #sentSize: GridSize | undefined;

  constructor(
    sessions: SessionClient,
    options: {
      renderer?: RendererPreference;
      spec?: SessionSpec;
      /** Opens a link the user Ctrl+clicked (Cmd+clicked on macOS). */
      openLink?: (url: string) => void;
      mac?: boolean;
      /** Shares parsing time with other panes; one per window by default. */
      output?: OutputScheduler;
      /** The prod guard for the saved host behind this terminal. */
      guard?: GuardSource;
    } = {},
  ) {
    this.#sessions = sessions;
    this.spec = options.spec ?? LOCAL;
    this.#openLink = options.openLink;
    this.#mac = options.mac ?? false;
    this.#webglRetired =
      chooseRenderer(options.renderer ?? "auto", globalThis.navigator?.userAgent ?? "") === "dom";
    this.terminal = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: FONT_FAMILY,
      fontSize: 13,
      scrollback: 10_000,
    });
    this.#output = (options.output ?? outputScheduler).lane(
      (chunk, parsed) => this.terminal.write(chunk, parsed),
      (bytes) => {
        const ready = this.#acks.add(bytes);
        if (ready > 0) {
          this.#acknowledge(ready);
        }
      },
    );
    this.terminal.loadAddon(this.#fit);
    this.terminal.loadAddon(this.#search);
    this.terminal.loadAddon(new Unicode11Addon());
    // Plain URLs in the output, and OSC 8 hyperlinks a program declares.
    const hint = this.#mac ? "Cmd+click to open" : "Ctrl+click to open";
    this.terminal.loadAddon(
      new WebLinksAddon((event, url) => this.#activateLink(event, url), {
        hover: () => this.terminal.element?.setAttribute("title", hint),
        leave: () => this.terminal.element?.removeAttribute("title"),
      }),
    );
    this.terminal.options.linkHandler = {
      activate: (event, url) => this.#activateLink(event, url),
      hover: () => this.terminal.element?.setAttribute("title", hint),
      leave: () => this.terminal.element?.removeAttribute("title"),
      allowNonHttpProtocols: false,
    };
    this.terminal.unicode.activeVersion = "11";

    this.#input = new InputQueue(
      {
        writeText: async (data) => (await this.#writable())?.write(data),
        writeBytes: async (data) => (await this.#writable())?.writeBytes(data),
      },
      ignoreEndedSession,
    );
    this.terminal.onTitleChange((title) => {
      // Remote programs set the title; keep it short and printable.
      this.#title = sanitizeTitle(title);
      this.#notify();
    });
    // Runs before xterm.js's own REP handler; returning true drops the sequence.
    this.terminal.parser.registerCsiHandler({ final: "b" }, (params) =>
      dropsRepeat(params, this.terminal.cols, this.terminal.rows),
    );
    this.#guardSource = options.guard;
    this.#guard = options.guard
      ? new CommandGuard(options.guard, {
          send: (data) => this.#input.push({ kind: "text", data }),
          screen: () => this.terminal.buffer.active,
          mark: () => this.terminal.registerMarker(0),
          ask: (request) => this.#askGuard(request),
          parsed: () => this.#parses,
          nextParse: (ms) =>
            new Promise((resolve) => {
              const timer = setTimeout(() => {
                this.#parseWaiters.delete(done);
                resolve(false);
              }, ms);
              const done = () => {
                clearTimeout(timer);
                resolve(true);
              };
              this.#parseWaiters.add(done);
            }),
        })
      : undefined;
    this.terminal.onWriteParsed(() => {
      this.#parses += 1;
      for (const waiter of this.#parseWaiters) waiter();
      this.#parseWaiters.clear();
    });
    this.terminal.onData((raw) => {
      const data = defusePaste(raw);
      if (this.#guard) {
        this.#guard.input(data);
      } else {
        this.#input.push({ kind: "text", data });
      }
    });
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

  /** Whether the prod guard checks this terminal's commands now. */
  get guarded(): boolean {
    return this.#guardSource?.active() ?? false;
  }

  /** For `useSyncExternalStore`: changes when the host's guard is turned on or off. */
  subscribeGuard = (listener: Listener): (() => void) =>
    this.#guardSource?.subscribe(listener) ?? (() => {});

  /** The prod guard's question, if any. For `useSyncExternalStore`. */
  getGuardRequest = (): GuardRequest | undefined => this.#guardRequest?.request;

  /** Answers the question `getGuardRequest` returned; true runs it. */
  answerGuard(run: boolean): void {
    const pending = this.#guardRequest;
    if (!pending) return;
    this.#guardRequest = undefined;
    pending.resolve(run);
    this.#notify();
    this.focus();
  }

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
      const { textarea } = this.terminal;
      if (textarea) {
        this.#stopImeFix = startUnannouncedCompositions(textarea);
        if (this.terminal.element) this.#pin = new TextareaPin(this.terminal.element, textarea);
      }
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
    if (this.#prompts.length > 0 || this.#guardRequest) return;
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
    this.#guard?.dispose();
    this.#guardRequest?.resolve(false);
    this.#guardRequest = undefined;
    this.#input.close();
    this.#stopImeFix?.();
    this.#pin?.dispose();
    this.#output.close();
    for (const pending of this.#prompts.splice(0)) pending.resolve(CANCEL);
    this.#session?.close().catch(ignoreEndedSession);
    this.#session = undefined;
    this.#settleOpened?.(undefined);
    this.terminal.dispose();
    this.#listeners.clear();
  }

  /** The session to send input to, waiting for one that is still opening. */
  async #writable(): Promise<TerminalSession | undefined> {
    return this.#session ?? (this.#opening ? this.#opened : undefined);
  }

  #open(): void {
    this.#opening = true;
    this.#opened = new Promise((settle) => {
      this.#settleOpened = settle;
    });
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
          this.#settleOpened?.(undefined);
          opened.close().catch(ignoreEndedSession);
          return;
        }
        this.#session = opened;
        this.#settleOpened?.(opened);
        if (this.#status.kind === "starting") {
          this.#setStatus({ kind: "running" });
        }
        this.#flushAcknowledgements();
        // The grid may have changed while the session was opening.
        this.#resizeSession();
      })
      .catch((error: unknown) => {
        this.#opening = false;
        this.#settleOpened?.(undefined);
        // Questions the connect left unanswered no longer matter.
        for (const pending of this.#prompts.splice(0)) pending.resolve(CANCEL);
        if (!this.#disposed) {
          const code = error instanceof SessionCommandError ? error.code : undefined;
          this.#setStatus({ kind: "failed", message: errorMessage(error), code });
        }
      });
  }

  /** Output is whatever a remote program printed: open only what passes. */
  #activateLink(event: MouseEvent, url: string): void {
    if (asksToOpen(event, this.#mac) && isOpenableLink(url)) this.#openLink?.(url);
  }

  #ask(prompt: PromptKind): Promise<AnswerSpec> {
    if (this.#disposed) return Promise.resolve(CANCEL);
    return new Promise((resolve) => {
      this.#prompts.push({ prompt, resolve });
      this.#notify();
    });
  }

  #askGuard(request: GuardRequest): Promise<boolean> {
    if (this.#disposed) return Promise.resolve(false);
    return new Promise((resolve) => {
      this.#guardRequest = { request, resolve };
      this.#notify();
    });
  }

  #write(data: Uint8Array): void {
    if (this.#disposed) {
      return;
    }
    this.#pin?.output(data.byteLength);
    this.#output.push(data);
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
