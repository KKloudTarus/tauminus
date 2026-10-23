// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import type { WebglAddon } from "@xterm/addon-webgl";
import { type IMarker, type ITheme, Terminal } from "@xterm/xterm";

import { SerializeAddon } from "@xterm/addon-serialize";

import { snapshot } from "@/features/recording/snapshot.ts";
import type { RecordingView } from "@/ipc/recordings.ts";
import { describePort } from "@/ipc/serial.ts";
import { formatEndpoint } from "@/features/ssh/destination.ts";
import { messageOf } from "@/features/hosts/hosts-store.ts";
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
import {
  CommandGuard,
  type GuardRequest,
  type GuardSource,
  atSecretPrompt,
  isPasswordPrompt,
} from "./command-guard.ts";
import { CommandRecorder, type RecordedCommand } from "./recorder.ts";
import { startUnannouncedCompositions } from "./ime.ts";
import { InputQueue, binaryStringToBytes } from "./input-queue.ts";
import { asksToOpen, isOpenableLink } from "./links.ts";
import { type Lane, type OutputScheduler, outputScheduler } from "./output-scheduler.ts";
import { asPaste, defusePaste, pastedText } from "./paste.ts";
import { type RendererPreference, chooseRenderer } from "./renderer.ts";
import { hardenParser } from "./repeat-guard.ts";
import { TextareaPin } from "./textarea-pin.ts";
import { webglBudget } from "./webgl-budget.ts";
import { type HighlightSource, Highlighter } from "./highlighter.ts";

/** What a pane shows about filling a password; see `TerminalController.getFill`. */
export type FillState =
  /** The login identity's name (`null` while the vault is locked) and `user@address`. */
  | { readonly kind: "offer"; readonly identity: string | null; readonly login: string }
  /** The host showed what the last fill typed: it may not have asked for a password. */
  | { readonly kind: "echoed"; readonly line: string }
  /** Why the last fill did not happen. */
  | { readonly kind: "failed"; readonly message: string; readonly line: string };

/** How long after a fill the line is looked at for an echo, in ms. */
const ECHO_CHECK_MS = 400;
/** How long a prompt line must stand before it is offered, in ms. */
const OFFER_MS = 60;
/** Characters of the cursor's line a prompt is looked for in. */
const TAIL = 256;
import { type FiredBatch, type TriggerSource, TriggerWatcher } from "./trigger-watcher.ts";

/** Delay before retrying an acknowledgement whose IPC call failed. */
const ACK_RETRY_MS = 100;

const FONT_FAMILY =
  '"JetBrains Mono", "Cascadia Code", "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace';

/**
 * What happened in a pane while no one looked at it, most urgent last:
 * output arrived, the shell asked for input (a new prompt or a bell), or
 * a command or the session failed.
 */
export type Activity = "idle" | "unread" | "waiting" | "error";

const ACTIVITY_RANK: Record<Activity, number> = { idle: 0, unread: 1, waiting: 2, error: 3 };

/** The most urgent of `activities`. */
export function mostUrgent(activities: Iterable<Activity>): Activity {
  let most: Activity = "idle";
  for (const activity of activities) {
    if (ACTIVITY_RANK[activity] > ACTIVITY_RANK[most]) most = activity;
  }
  return most;
}

export type TerminalStatus =
  | { kind: "starting" }
  | { kind: "running" }
  | { kind: "exited"; code: number | null }
  /** The connection was lost; a new one is tried at `retryAt` (ms since epoch). */
  | {
      kind: "reconnecting";
      attempt: number;
      retryAt: number;
      /** Waiting for other panes of the host to finish connecting first. */
      queued?: boolean;
    }
  | {
      kind: "failed";
      message: string;
      code: ErrorCode | undefined;
      /** Reconnecting gave up: the screen holds the lost session's work. */
      afterLoss?: boolean;
    };

const CANCEL: AnswerSpec = { kind: "cancel" };
/** How long each try waits after a connection is lost, in ms. */
const RECONNECT_DELAYS = [1000, 2000, 5000, 10_000, 15_000, 30_000];
/** About ten minutes of tries. */
const MAX_RECONNECTS = 25;
/** A session up this long, in ms, was stable: its next loss starts the tries over. */
const STABLE_MS = 60_000;
/** Panes of one host connecting again at the same time: under sshd's `MaxStartups` (10). */
const RECONNECTS_PER_HOST = 6;
/** Losses within this window, in ms, past {@link MAX_LOSSES} give up. */
const LOSS_WINDOW_MS = 30 * 60_000;
const MAX_LOSSES = 10;
/** Back to the window after this long away, in ms, a running pane checks its link. */
const AWAY_MS = 15_000;
/** How long, in ms, a host's refusal stops its other panes from asking. */
const REFUSAL_MS = 60_000;
/** Focus and `online` act at most this often per pane, in ms. */
const WAKE_MS = 5000;

/** Reconnects in flight, by host; shared by every pane. */
const inFlight = new Map<string, number>();
/** Panes waiting for a slot, by host, oldest first. */
const waiting = new Map<string, (() => boolean)[]>();

/** Gives a slot of `key` back; the next pane in line takes it. */
function freeSlot(key: string): void {
  const left = (inFlight.get(key) ?? 1) - 1;
  if (left > 0) inFlight.set(key, left);
  else inFlight.delete(key);
  // After this call returns; one that no longer needs the slot passes it on.
  queueMicrotask(() => handOn(key));
}

/** Gives `key`'s free slot to the first pane in line that takes it. */
function handOn(key: string): void {
  const queue = waiting.get(key) ?? [];
  while ((inFlight.get(key) ?? 0) < RECONNECTS_PER_HOST) {
    const next = queue.shift();
    if (!next) break;
    next();
  }
  if (queue.length === 0) waiting.delete(key);
}
/** When this window last lost focus; 0 if never. */
let windowBlurredAt = 0;
globalThis.addEventListener?.("blur", () => {
  windowBlurredAt = Date.now();
});
/** The last refusal a reconnect got, by host. */
const refusals = new Map<string, { at: number; message: string; code: ErrorCode | undefined }>();

/**
 * Whether a lost session of this kind is worth connecting again. Mosh
 * keeps its own session across networks; its client exits only when the
 * server is gone.
 */
function reconnects(spec: SessionSpec): boolean {
  return spec.kind === "ssh" || spec.kind === "host" || spec.kind === "telnet";
}

/** Which host a reconnect knocks on. */
function reconnectKey(spec: SessionSpec): string {
  switch (spec.kind) {
    case "host":
    case "telnet":
    case "mosh":
      return `host:${spec.hostId}`;
    case "ssh": {
      const { user, host, port } = spec.target.destination;
      return `ssh:${user}@${host}:${port}`;
    }
    default:
      return spec.kind;
  }
}

/** `ms`, give or take a quarter. */
function jittered(ms: number): number {
  return Math.round(ms * (0.75 + Math.random() * 0.5));
}
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
  #onInput: ((data: string) => void) | undefined;
  /** Whether the pane is on screen; what happens then is seen. */
  #watched = true;
  #activity: Activity = "idle";
  #recording: RecordingView | undefined;
  #serialize: SerializeAddon | undefined;
  /** Tries since the connection was lost, or last stayed up a while. */
  #reconnects = 0;
  #reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  /** When the session last came up, for telling a flapping link from a stable one. */
  #connectedAt = 0;
  /** Counts opens; an open's callbacks act only while it is the latest. */
  #openEpoch = 0;
  /** The host whose reconnect slot this pane holds, while it holds one. */
  #slot: string | undefined;
  /** A try that came from a wake, not the timer: it does not count. */
  #wakeTry = false;
  #lastWake = 0;
  /** When the session was lost, for the last {@link LOSS_WINDOW_MS}. */
  #losses: number[] = [];
  /** The open in progress follows a lost session, whose screen stays. */
  #afterLoss = false;
  /** Waking, or the network coming back: try now, or check the link. */
  readonly #onWake = (event: Event) => this.#wake(event.type);
  /** Called when this pane's turn to connect again comes. */
  readonly #turn = (): boolean => this.#attempt();
  /** Set while a pointer event is being handled, so its keys stay here. */
  #fromPointer = false;
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
  readonly #recorder: CommandRecorder | undefined;
  readonly #highlighter: Highlighter | undefined;
  readonly #watcher: TriggerWatcher | undefined;
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
      /** Where the commands this terminal runs are recorded. */
      record?: (entry: RecordedCommand) => void;
      /** The highlight rules for this pane's host, or the defaults. */
      highlights?: HighlightSource;
      /** The trigger rules for this pane's host, and what to do when one fires. */
      triggers?: { source: TriggerSource; fired: (batch: FiredBatch) => void };
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
        this.#parsedBytes += bytes;
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
        fillPassword: async (seen) => {
          const session = await this.#writable();
          if (!session?.fillPassword) throw new Error("this session has no saved host");
          await session.fillPassword(seen);
        },
      },
      ignoreEndedSession,
    );
    this.terminal.onTitleChange((title) => {
      // Remote programs set the title; keep it short and printable.
      this.#title = sanitizeTitle(title);
      this.#notify();
    });
    hardenParser(this.terminal);
    this.#guardSource = options.guard;
    this.#guard = options.guard
      ? new CommandGuard(options.guard, {
          // The history hears what the guard lets through, not what it holds back.
          send: (data) => {
            this.#recorder?.input(data);
            this.#input.push({ kind: "text", data });
          },
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
    this.#highlighter = options.highlights
      ? new Highlighter(this.terminal, options.highlights)
      : undefined;
    this.#watcher = options.triggers
      ? new TriggerWatcher(this.terminal, options.triggers.source, options.triggers.fired)
      : undefined;
    this.terminal.onWriteParsed(() => {
      this.#checkFill();
      this.#parses += 1;
      for (const waiter of this.#parseWaiters) waiter();
      this.#parseWaiters.clear();
    });
    const record = options.record;
    this.#recorder = record
      ? new CommandRecorder({
          screen: () => this.terminal.buffer.active,
          mark: () => this.terminal.registerMarker(0),
          record,
          now: () => Date.now(),
        })
      : undefined;
    // Shell integration (OSC 133): where prompts, commands, and output start,
    // and how each command ended. Other handlers still see the sequence.
    this.terminal.parser.registerOscHandler(133, (data) => {
      this.#recorder?.shellMark(data);
      // A new prompt waits for input; a command that ended with a
      // non-zero status failed.
      if (data === "A" || data.startsWith("A;")) this.#mark("waiting");
      const ended = /^D;(-?\d+)/u.exec(data);
      if (ended && Number(ended[1]) !== 0) this.#mark("error");
      return false;
    });
    this.terminal.onBell(() => this.#mark("waiting"));
    // Back from sleep, or on a network again: no need to wait for the timer.
    globalThis.addEventListener?.("online", this.#onWake);
    globalThis.addEventListener?.("focus", this.#onWake);
    this.terminal.onData((raw) => {
      const data = defusePaste(raw);
      this.#noteTyping(data);
      this.#checkFill();
      // Wheel scrolling and Alt+click send arrow keys worked out from this
      // pane's screen; they mean nothing in another.
      if (!this.#fromPointer) this.#onInput?.(data);
      if (data === "\u0003") this.interrupt();
      else this.type(data);
    });
    this.terminal.onBinary((data) =>
      this.#input.push({ kind: "bytes", data: binaryStringToBytes(data) }),
    );
  }

  /** What happened here while it was off screen. For `useSyncExternalStore`. */
  getActivity = (): Activity => this.#activity;

  /** On screen or not: coming on screen marks everything seen. */
  setWatched(watched: boolean): void {
    if (this.#watched === watched) return;
    this.#watched = watched;
    if (watched) this.#activity = "idle";
    this.#notify();
  }

  /** Raises the activity while off screen; O(1) per call, notifies on change only. */
  #mark(activity: Activity): void {
    if (this.#watched || ACTIVITY_RANK[activity] <= ACTIVITY_RANK[this.#activity]) return;
    this.#activity = activity;
    this.#notify();
  }

  /** Connects again at once instead of waiting for the next try. */
  reconnectNow(): void {
    this.#wakeTry = false;
    this.#attempt();
  }

  /** Gives up connecting again; the pane shows the connection closed. */
  stopReconnecting(): void {
    if (this.#status.kind !== "reconnecting") return;
    clearTimeout(this.#reconnectTimer);
    this.#reconnectTimer = undefined;
    this.#reconnects = 0;
    // A try still in flight is closed when it opens, and its questions
    // are withdrawn.
    this.#openEpoch += 1;
    this.#opening = false;
    this.#settleOpened?.(undefined);
    for (const pending of this.#prompts.splice(0)) pending.resolve(CANCEL);
    this.#leaveLine();
    this.#setStatus({ kind: "exited", code: null });
  }

  /**
   * One try, if this host has a free slot: at most
   * {@link RECONNECTS_PER_HOST} panes of one host connect at a time, so a
   * wake does not flood its sshd, and a host that just refused one pane is
   * not asked by the others.
   */
  /** Returns whether this pane took a slot and is connecting. */
  #attempt(): boolean {
    if (this.#status.kind !== "reconnecting" || this.#opening || this.#disposed) return false;
    const key = reconnectKey(this.spec);
    const refused = refusals.get(key);
    if (refused && Date.now() - refused.at < REFUSAL_MS) {
      this.#giveUp(refused.message, refused.code);
      return false;
    }
    if ((inFlight.get(key) ?? 0) >= RECONNECTS_PER_HOST) {
      // Waits its turn, in order; that is not a try.
      const queue = waiting.get(key) ?? [];
      if (!queue.includes(this.#turn)) queue.push(this.#turn);
      waiting.set(key, queue);
      if (!this.#status.queued) this.#setStatus({ ...this.#status, queued: true });
      return false;
    }
    inFlight.set(key, (inFlight.get(key) ?? 0) + 1);
    this.#slot = key;
    if (this.#status.queued) {
      const { queued: _, ...status } = this.#status;
      this.#setStatus(status);
    }
    if (!this.#wakeTry) {
      clearTimeout(this.#reconnectTimer);
      this.#reconnectTimer = undefined;
    }
    this.#open();
    return true;
  }

  /**
   * The next counted try, in `wait` ms. One that comes due while a wake's
   * try is still connecting waits for it instead of being spent.
   */
  #armTimer(wait: number): void {
    clearTimeout(this.#reconnectTimer);
    this.#reconnectTimer = setTimeout(() => {
      this.#reconnectTimer = undefined;
      if (this.#opening) {
        this.#armTimer(jittered(1000));
        return;
      }
      this.reconnectNow();
    }, wait);
  }

  /** Leaves the line for a slot. A slot taken stays with its connect. */
  #leaveLine(): void {
    for (const [key, queue] of waiting) {
      const at = queue.indexOf(this.#turn);
      if (at >= 0) queue.splice(at, 1);
      if (queue.length === 0) waiting.delete(key);
    }
  }

  /**
   * The window came back to the front, or the network did. A pane waiting
   * to reconnect tries now, without using up a try; a running one checks
   * its link, which a sleep may have killed. At most once per
   * {@link WAKE_MS}, as focus comes and goes all day.
   */
  #wake(type: string): void {
    const now = Date.now();
    if (now - this.#lastWake < WAKE_MS || this.#disposed) return;
    if (this.#status.kind === "reconnecting") {
      this.#lastWake = now;
      this.#wakeTry = true;
      this.#attempt();
    } else if (
      this.#status.kind === "running" &&
      reconnects(this.spec) &&
      // A quick look away is no sleep: keepalives cover it. It does not
      // use up the throttle either, so `online` right after still checks.
      (type === "online" || now - windowBlurredAt >= AWAY_MS)
    ) {
      this.#lastWake = now;
      this.#session?.probe().catch(ignoreEndedSession);
    }
  }

  #giveUp(message: string, code: ErrorCode | undefined): void {
    clearTimeout(this.#reconnectTimer);
    this.#reconnectTimer = undefined;
    this.#reconnects = 0;
    this.#setStatus({ kind: "failed", message, code, afterLoss: true });
  }

  /**
   * Waits, then connects again: 1, 2, 5, 10, 15, then every 30 s, for
   * about ten minutes. The terminal keeps what it showed; a tmux session
   * draws itself again.
   */
  #reconnectLater(): void {
    this.#reconnects += 1;
    if (this.#reconnects > MAX_RECONNECTS) {
      this.#giveUp("the connection was lost", "connect_failed");
      return;
    }
    // Spread out, so panes that lost one network do not all knock at once.
    const wait = jittered(
      RECONNECT_DELAYS[Math.min(this.#reconnects - 1, RECONNECT_DELAYS.length - 1)] ?? 30_000,
    );
    this.#armTimer(wait);
    this.#setStatus({
      kind: "reconnecting",
      attempt: this.#reconnects,
      retryAt: Date.now() + wait,
    });
  }

  /** Hears what the user types here, for broadcast input. */
  setInputListener(listener: ((data: string) => void) | undefined): void {
    this.#onInput = listener;
  }

  /** Whether the shell is asking for a password or another secret now. */
  get atSecretPrompt(): boolean {
    return atSecretPrompt(this.terminal.buffer.active);
  }

  /**
   * Types text a trigger sends, as `type` does (through the prod guard,
   * into the history, not broadcast), unless the shell is asking for a
   * secret or a full-screen program has the screen: there, keys would be a
   * password or an editor's commands. `false` when it was not sent.
   */
  sendFromTrigger(data: string): boolean {
    if (!this.canSendFromTrigger) return false;
    this.type(data);
    return true;
  }

  /**
   * Whether a trigger may type now: not at a password prompt, not into a
   * full-screen program, not over a line the user is typing (with Enter,
   * it would run).
   */
  get canSendFromTrigger(): boolean {
    return !(
      this.terminal.buffer.active.type === "alternate" ||
      this.atSecretPrompt ||
      this.#typing
    );
  }

  /**
   * Whether the user has typed on the current line: keys since the last
   * Enter, Ctrl+C, Ctrl+D, or Ctrl+U.
   */
  #typing = false;

  #noteTyping(data: string): void {
    const end = Math.max(
      data.lastIndexOf("\r"),
      data.lastIndexOf("\n"),
      data.lastIndexOf("\u0003"),
      data.lastIndexOf("\u0004"),
      data.lastIndexOf("\u0015"),
    );
    const after = data.slice(end + 1);
    // Keys that type something: not a lone escape sequence such as an arrow.
    // oxlint-disable-next-line no-control-regex -- escape sequences are what is left out
    const typed = after.replaceAll(/\u001b(\[[0-9;?]*[ -/]*[@-~]|O.|.)/gu, "").length > 0;
    if (end >= 0) this.#typing = typed;
    else if (typed) this.#typing = true;
  }

  /**
   * Types keys another pane's user typed: a paste is bracketed or not as
   * this pane's shell asked, whatever the other one did.
   */
  typeShared(data: string): void {
    const pasted = pastedText(data);
    this.type(
      pasted === undefined ? data : asPaste(pasted, this.terminal.modes.bracketedPasteMode),
    );
  }

  /** Ctrl+C at once, ahead of input the prod guard holds for a check. */
  interrupt(): void {
    if (this.#guard) {
      this.#guard.interrupt();
    } else {
      this.#recorder?.input("\u0003");
      this.#input.push({ kind: "text", data: "\u0003" });
    }
  }

  /**
   * Sends `data` as typed keys: through the prod guard when the host has
   * one, which asks before anything with Enter in it runs, and into the
   * history as the user's own input would go.
   */
  type(data: string): void {
    // Broadcast keys and snippets type on the line as the user's own do.
    this.#noteTyping(data);
    this.#checkFill();
    if (this.#guard) {
      this.#guard.input(data);
    } else {
      this.#recorder?.input(data);
      this.#input.push({ kind: "text", data });
    }
  }

  get status(): TerminalStatus {
    return this.#status;
  }

  /** The backend's id for the session running now, if one is. */
  get sessionId(): number | undefined {
    return this.#session?.id;
  }

  /**
   * What the pane shows about filling a password: an offer at a prompt
   * (the saved host's login identity, its name `null` while the vault is
   * locked), a warning that the host showed what was typed, or why a fill
   * did not happen. For `useSyncExternalStore`.
   */
  getFill = (): FillState | undefined => this.#fill;

  /**
   * Types the saved host's login password at the prompt the pane shows,
   * without Enter: the backend checks the prompt itself and sends it, the
   * view never sees it. In turn with keys already queued. `"locked"` when
   * the vault must be unlocked first.
   */
  async fillPassword(): Promise<"filled" | "locked" | "failed" | "none"> {
    const prompt = this.#fillPrompt();
    if (this.#fill?.kind !== "offer" || prompt === undefined || prompt.key !== this.#offered) {
      return "none";
    }
    const seen = this.#parsedBytes;
    try {
      await new Promise<void>((resolve, reject) =>
        this.#input.push({
          kind: "secret",
          seen,
          done: (error) => (error === undefined ? resolve() : reject(error)),
        }),
      );
    } catch (error) {
      if (error instanceof SessionCommandError && error.code === "vault_locked") return "locked";
      if (error instanceof SessionCommandError && error.code === "cancelled") {
        // Output came since the prompt was read: look again, and offer
        // again if it still stands.
        this.#offered = undefined;
        this.#checkFill();
        return "failed";
      }
      this.#fill = { kind: "failed", message: messageOf(error), line: prompt.key };
      this.#notify();
      return "failed";
    }
    this.#filled?.dispose();
    this.#filled = this.#markRow(prompt.row);
    this.#filledKey = prompt.key;
    this.#fill = undefined;
    this.#notify();
    // A real password prompt shows nothing of what is typed (or `*`s). A
    // host that shows it was not asking for a password: the user should
    // know, and clear the line (Ctrl+U) before pressing Enter.
    setTimeout(() => {
      if (this.#disposed) return;
      const now = this.#cursorTail();
      const added = now.text.startsWith(prompt.text)
        ? now.text.slice(prompt.text.length)
        : now.text;
      // The prompt's own row, by its marker: with scrollback full, another
      // line comes to sit at the same index.
      const filled = this.#filled;
      const same = filled !== undefined && !filled.isDisposed && filled.line === now.row;
      if (same && /[^\s*•●]/u.test(added)) {
        this.#fill = { kind: "echoed", line: this.#cursorTail().key };
        this.#notify();
      }
    }, ECHO_CHECK_MS);
    return "filled";
  }

  /** Bytes of the current session's output parsed so far. */
  #parsedBytes = 0;
  #fill: FillState | undefined;
  /** The prompt the offer is for. */
  #offered: string | undefined;
  /** The prompt last filled, by a marker on its row: not offered again. */
  #filled: IMarker | undefined;
  #filledKey: string | undefined;
  #offerTimer: ReturnType<typeof setTimeout> | undefined;

  #markRow(row: number): IMarker | undefined {
    const buffer = this.terminal.buffer.active;
    return this.terminal.registerMarker(row - (buffer.baseY + buffer.cursorY));
  }

  /**
   * The last [`TAIL`] characters of the cursor's line, where it starts,
   * and a key for it. Reads only the rows the tail is in, however long the
   * line: O(TAIL + rows to its start).
   */
  #cursorTail(): { key: string; text: string; row: number } {
    const buffer = this.terminal.buffer.active;
    const cursor = buffer.baseY + buffer.cursorY;
    let start = cursor;
    for (let back = 0; back < 512 && start > 0 && buffer.getLine(start)?.isWrapped; back += 1) {
      start -= 1;
    }
    let text = "";
    for (let row = cursor; row >= start && text.length < TAIL; row -= 1) {
      text = (buffer.getLine(row)?.translateToString(row === cursor) ?? "") + text;
    }
    text = text.slice(-TAIL);
    return { key: `${start}:${text}`, text, row: start };
  }

  /** The prompt the vault may fill now, if the pane shows one. */
  #fillPrompt(): { key: string; text: string; row: number } | undefined {
    if (
      !this.#session?.fillOffer ||
      this.#status.kind !== "running" ||
      this.terminal.buffer.active.type !== "normal" ||
      this.#typing
    ) {
      return undefined;
    }
    const tail = this.#cursorTail();
    if (!isPasswordPrompt(tail.text)) return undefined;
    // The same prompt, filled already: its row's marker still there.
    const filled = this.#filled;
    if (
      filled !== undefined &&
      !filled.isDisposed &&
      filled.line === tail.row &&
      this.#filledKey === tail.key
    ) {
      return undefined;
    }
    return tail;
  }

  /**
   * Offers, or takes back the offer, as the cursor's line changes. The
   * backend is asked once the line has stood for [`OFFER_MS`]: a prompt
   * waits for its answer, a log line scrolling past does not. A warning
   * or a failure stays until the line changes. O(tail).
   */
  #checkFill(): void {
    const prompt = this.#fillPrompt();
    const line = prompt?.key ?? this.#cursorTail().key;
    if (this.#fill !== undefined && this.#fill.kind !== "offer" && this.#fill.line !== line) {
      this.#fill = undefined;
      this.#notify();
    }
    if (
      prompt === undefined &&
      this.#filled !== undefined &&
      this.#cursorTail().row !== this.#filled.line
    ) {
      // The cursor left the filled prompt: a later one, however alike, is new.
      this.#filled.dispose();
      this.#filled = undefined;
      this.#filledKey = undefined;
    }
    const key = prompt?.key;
    if (key === this.#offered) return;
    this.#offered = key;
    clearTimeout(this.#offerTimer);
    if (this.#fill?.kind === "offer") {
      this.#fill = undefined;
      this.#notify();
    }
    const session = this.#session;
    if (key === undefined || !session?.fillOffer) return;
    this.#offerTimer = setTimeout(() => {
      if (this.#offered !== key || this.#fillPrompt()?.key !== key) return;
      session
        .fillOffer?.()
        .then((offer) => {
          if (this.#offered !== key || !offer) return;
          this.#fill = { kind: "offer", identity: offer.identity, login: offer.login };
          this.#notify();
        })
        .catch(() => {});
    }, OFFER_MS);
  }

  /** The recording of this pane in progress, if any. For `useSyncExternalStore`. */
  getRecording = (): RecordingView | undefined => this.#recording;

  /** Marks the pane as recording, or not, for its badge. */
  setRecording(recording: RecordingView | undefined): void {
    if (this.#recording === recording) return;
    this.#recording = recording;
    this.#notify();
  }

  /**
   * The screen as it is now, as text that draws it again: where a recording
   * begun now starts. The screen only: scrollback from before the user
   * asked to record stays out of it. O(screen).
   */
  screenSnapshot(): string {
    if (!this.#serialize) {
      this.#serialize = new SerializeAddon();
      this.terminal.loadAddon(this.#serialize);
    }
    // Between escape sequences, the state that follows is exact; in the
    // middle of one, the screen alone is still the best start there is.
    return (
      snapshot(this.terminal, this.#serialize, 0) ?? this.#serialize.serialize({ scrollback: 0 })
    );
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
    // After a lost connection the screen is the user's work: keep it,
    // through as many retries as it takes.
    this.#afterLoss = this.#status.afterLoss === true;
    if (!this.#afterLoss) this.terminal.reset();
    this.#reconnects = 0;
    this.#setStatus({ kind: "starting" });
    this.#open();
  }

  /** What to call this terminal before a program sets a title. */
  get defaultTitle(): string {
    switch (this.spec.kind) {
      case "host":
      case "telnet":
      case "mosh":
        return this.spec.label;
      case "ssh":
        return formatEndpoint(this.spec.target.destination);
      case "serial":
        return describePort(this.spec.port);
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

  /** The highlight sets, or the host's list of them, changed. */
  refreshHighlights(): void {
    this.#highlighter?.refresh();
    this.#watcher?.refresh();
  }

  /** Re-reads the theme tokens, after the app switches light or dark. */
  refreshTheme(): void {
    if (this.#host) {
      this.terminal.options.theme = themeFrom(this.#host);
    }
  }

  /**
   * Marks the keys a pointer event makes, so they are not broadcast: only
   * while that event is dispatched. Capture sets the flag before xterm.js
   * turns the event into keys, and bubbling clears it after, so text an
   * IME commits on its own timer just before a click is still broadcast.
   * A timer clears it too, in case xterm.js stops the event.
   */
  #watchPointer(): void {
    const element = this.terminal.element;
    if (!element) return;
    const set = () => {
      this.#fromPointer = true;
      setTimeout(clear, 0);
    };
    const clear = () => {
      this.#fromPointer = false;
    };
    for (const type of ["wheel", "mousedown", "mouseup", "click"]) {
      element.addEventListener(type, set, { capture: true, passive: true });
      element.addEventListener(type, clear, { passive: true });
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
      this.#watchPointer();
      const { textarea } = this.terminal;
      if (textarea) {
        this.#stopImeFix = startUnannouncedCompositions(textarea);
        if (this.terminal.element) this.#pin = new TextareaPin(this.terminal.element, textarea);
      }
    }
    this.#acquireWebgl();
    this.#highlighter?.wake();
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
    clearTimeout(this.#reconnectTimer);
    globalThis.removeEventListener?.("online", this.#onWake);
    globalThis.removeEventListener?.("focus", this.#onWake);
    this.#openEpoch += 1;
    this.#leaveLine();
    this.#retireWebgl();
    this.#guard?.dispose();
    this.#highlighter?.dispose();
    this.#watcher?.dispose();
    // A tab closed with a command running keeps what it printed so far.
    this.#recorder?.flush();
    this.#recorder?.dispose();
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
    // Keys typed at a lost session were meant for it, not for the next.
    if (this.#status.kind === "reconnecting") return undefined;
    return this.#session ?? (this.#opening ? this.#opened : undefined);
  }

  #open(): void {
    // The slot is this connect's until it settles, even if the pane stops
    // waiting for it: the backend keeps signing in until then.
    const slot = this.#slot;
    this.#slot = undefined;
    const release = () => {
      if (slot !== undefined) freeSlot(slot);
    };
    const epoch = ++this.#openEpoch;
    const current = () => epoch === this.#openEpoch && !this.#disposed;
    /** This session already ended, perhaps before `open` returned. */
    let ended = false;
    this.#opening = true;
    this.#opened = new Promise((settle) => {
      this.#settleOpened = settle;
    });
    this.#sentSize = { cols: this.terminal.cols, rows: this.terminal.rows };
    // The backend counts output per session, from its first byte, which
    // may be parsed before `open` returns.
    this.#parsedBytes = 0;
    this.#sessions
      .open(this.spec, this.#sentSize, {
        onPrompt: (prompt) => this.#ask(prompt),
        onOutput: (data) => this.#write(data),
        onExit: (code, lost) => {
          ended = true;
          if (!current()) return;
          this.#session = undefined;
          this.#recorder?.flush();
          if (lost && reconnects(this.spec)) {
            this.#lost();
          } else {
            this.#setStatus({ kind: "exited", code });
          }
        },
      })
      .then((opened) => {
        release();
        if (!current()) {
          // Stopped or gone while connecting.
          opened.close().catch(ignoreEndedSession);
          return;
        }
        this.#opening = false;
        if (ended) {
          // Lost or exited before `open` returned; that was handled, but a
          // try that came due meanwhile found this one still opening.
          this.#settleOpened?.(undefined);
          if (this.#status.kind === "reconnecting" && this.#reconnectTimer === undefined) {
            this.#armTimer(jittered(1000));
          }
          return;
        }
        this.#session = opened;
        this.#settleOpened?.(opened);
        if (this.#status.kind === "starting" || this.#status.kind === "reconnecting") {
          if (this.#reconnects > 0) {
            this.terminal.write("\u001b[2m[connected again]\u001b[0m\r\n");
          }
          refusals.delete(reconnectKey(this.spec));
          this.#wakeTry = false;
          this.#connectedAt = Date.now();
          this.#afterLoss = false;
          this.#setStatus({ kind: "running" });
        }
        this.#flushAcknowledgements();
        // The grid may have changed while the session was opening.
        this.#resizeSession();
      })
      .catch((error: unknown) => {
        release();
        if (!current()) return;
        this.#opening = false;
        this.#settleOpened?.(undefined);
        // Questions the connect left unanswered no longer matter.
        for (const pending of this.#prompts.splice(0)) pending.resolve(CANCEL);
        const code = error instanceof SessionCommandError ? error.code : undefined;
        const message = errorMessage(error);
        if (this.#status.kind === "reconnecting") {
          // Still offline: try again later. A refusal would only be asked
          // again, of this pane and of the others on the host, so it stops.
          if (code === "connect_failed") {
            const wake = this.#wakeTry;
            this.#wakeTry = false;
            if (!(wake && this.#reconnectTimer !== undefined)) this.#reconnectLater();
            return;
          }
          // Declining a question decides for this pane only.
          if (code !== "cancelled") {
            refusals.set(reconnectKey(this.spec), { at: Date.now(), message, code });
          }
          this.#giveUp(message, code);
          return;
        }
        this.#reconnects = 0;
        this.#setStatus({
          kind: "failed",
          message,
          code,
          ...(this.#afterLoss ? { afterLoss: true } : {}),
        });
      });
  }

  /**
   * The link went away. The screen stays; what the old session left open
   * (a control string, a link, the alternate screen, mouse reports, paste
   * brackets) is closed, a question for it is withdrawn, and the next
   * connect is planned. A link that had stayed up a while starts the tries
   * over; one that drops right after connecting keeps counting, so a
   * flapping host is given up on.
   */
  #lost(): void {
    // Leaving the alternate screen also restores the cursor saved on
    // entering it; on the normal screen that would jump to the top.
    const alternate = this.terminal.buffer.active.type === "alternate" ? "\u001b[?1049l" : "";
    this.terminal.write(
      `\u0018\u001b]8;;\u0007${alternate}\u001b[!p\u001b[?1000l\u001b[?1002l\u001b[?1003l\u001b[?1006l\u001b[?1l\u001b[?2004l` +
        "\r\n\u001b[2m[connection lost]\u001b[0m\r\n",
    );
    this.#guardRequest?.resolve(false);
    this.#guardRequest = undefined;
    this.#guard?.reset();
    this.#input.clear();
    const now = Date.now();
    this.#losses = [...this.#losses.filter((at) => now - at < LOSS_WINDOW_MS), now];
    // A link that keeps dropping, however long each session lasts, is
    // given up on.
    if (this.#losses.length > MAX_LOSSES) {
      this.#losses = [];
      this.#giveUp("the connection keeps dropping", "connect_failed");
      return;
    }
    if (now - this.#connectedAt >= STABLE_MS) this.#reconnects = 0;
    this.#reconnectLater();
  }

  /** Output is whatever a remote program printed: open only what passes. */
  #activateLink(event: MouseEvent, url: string): void {
    if (asksToOpen(event, this.#mac) && isOpenableLink(url)) this.#openLink?.(url);
  }

  #ask(prompt: PromptKind): Promise<AnswerSpec> {
    if (this.#disposed) return Promise.resolve(CANCEL);
    return new Promise((resolve) => {
      this.#prompts.push({ prompt, resolve });
      // A connect waiting on a password or a host key waits on the user.
      this.#mark("waiting");
      this.#notify();
    });
  }

  #askGuard(request: GuardRequest): Promise<boolean> {
    if (this.#disposed) return Promise.resolve(false);
    return new Promise((resolve) => {
      this.#guardRequest = { request, resolve };
      this.#mark("waiting");
      this.#notify();
    });
  }

  #write(data: Uint8Array): void {
    if (this.#disposed) {
      return;
    }
    this.#mark("unread");
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
    // A recording ends with the session it records.
    if (status.kind !== "running") {
      this.#recording = undefined;
      this.#fill = undefined;
      this.#offered = undefined;
      clearTimeout(this.#offerTimer);
    }
    if (status.kind === "failed" || (status.kind === "exited" && status.code !== 0)) {
      this.#mark("error");
    }
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
