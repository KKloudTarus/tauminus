// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import {
  type AnswerSpec,
  type CommandError,
  type EndpointSpec,
  type ErrorCode,
  type GridSize,
  type PromptKind,
  type PromptRequest,
  type RouteSpec,
  type SessionExit,
  type SshTargetSpec,
  type SerialSpec,
  type TabWindowView,
  type TopologyView,
  commands,
} from "./bindings.ts";

export type {
  AnswerSpec,
  EndpointSpec,
  ErrorCode,
  GridSize,
  PromptKind,
  RouteSpec,
  SshTargetSpec,
  TabWindowView,
  TopologyView,
};

/**
 * What a session runs: the local shell, a shell on an SSH host, or a saved
 * host. A saved host connects by id, so its settings, including a
 * ProxyCommand, come from the store and never from the view; `target` is
 * for showing where it goes.
 */
export type SessionSpec =
  | { kind: "local" }
  | { kind: "ssh"; target: SshTargetSpec }
  | { kind: "host"; hostId: string; label: string; target: SshTargetSpec }
  /** A saved Telnet host: clear text, no host key, no prompts. */
  | { kind: "telnet"; hostId: string; label: string }
  /** A saved host signed in to over SSH, its terminal carried by Mosh. */
  | { kind: "mosh"; hostId: string; label: string }
  /** A serial port and its line settings. */
  | { kind: "serial"; port: SerialSpec }
  /**
   * A saved host's tmux session as tabs (ADR 0024). Without `control`, it
   * starts the control client and shows the first window; with it, the
   * pane of one window that client showed.
   */
  | {
      kind: "tmux";
      hostId: string;
      label: string;
      control?: number;
      window?: number;
      pane?: number;
    };

/** What a tmux session shown as tabs tells the app that opened it. */
export interface TmuxEvents {
  /** The client started, showing `first` here; `rest` want tabs of their own. */
  opened(control: number, first: TabWindowView, rest: TabWindowView[]): void;
  topology(event: TopologyView): void;
  /** The host's tmux cannot show tabs; the session joined it the plain way. */
  fellBack(reason: string): void;
}

/** Asks the user during a connect. Must settle: answer `cancel` rather than hang. */
/**
 * Answers a question an SSH connect raised. `withdrawn` aborts when the
 * backend no longer asks it (a security key answered, the connect gave
 * up): the view hides it, and its answer goes nowhere.
 */
export type PromptHandler = (prompt: PromptKind, withdrawn?: AbortSignal) => Promise<AnswerSpec>;

/**
 * A connect's prompt channel: each question goes to `ask`, its answer
 * back to the backend, and a `withdrawn` message aborts the one it names.
 * O(1) per message.
 */
export function promptChannel(ask: PromptHandler | undefined): Channel<PromptRequest> {
  const channel = new Channel<PromptRequest>();
  const open = new Map<number, AbortController>();
  channel.onmessage = ({ id, prompt }) => {
    if (prompt.kind === "withdrawn") {
      open.get(id)?.abort();
      open.delete(id);
      return;
    }
    const withdrawn = new AbortController();
    open.set(id, withdrawn);
    (ask ?? (async () => CANCEL))(prompt, withdrawn.signal)
      .catch(() => CANCEL)
      .then(async (answer) => {
        if (withdrawn.signal.aborted) return;
        open.delete(id);
        await commands.sshPromptReply(id, answer);
      })
      .catch((error: unknown) => console.warn("prompt reply failed", error));
  };
  return channel;
}

/** A failed session command, carrying the stable code from the backend. */
export class SessionCommandError extends Error {
  readonly code: ErrorCode;

  constructor(error: CommandError) {
    super(error.message);
    this.name = "SessionCommandError";
    this.code = error.code;
  }
}

export interface SessionEvents {
  /** Called with each batch of terminal output, in order. */
  onOutput(data: Uint8Array): void;
  /**
   * Called once when the session ends. `lost` says the connection went
   * away first: the remote shell may still be there to connect to again.
   */
  onExit(exitCode: number | null, lost?: boolean): void;
  /** Called for each question an SSH connect raises. Without it, every prompt is declined. */
  onPrompt?: PromptHandler;
  /** For a `tmux` session that starts its control client. */
  tmux?: TmuxEvents;
}

export interface TerminalSession {
  readonly id: number;
  write(data: string): Promise<void>;
  /** Bytes that are not valid UTF-8, such as legacy mouse reports. */
  writeBytes(data: Uint8Array): Promise<void>;
  resize(size: GridSize): Promise<void>;
  /** Reports output bytes the terminal has parsed, so the backend can resume. */
  acknowledge(bytes: number): Promise<void>;
  /** Checks the link still answers; a dead one ends the session as lost. */
  probe(): Promise<void>;
  close(): Promise<void>;
  /**
   * Whether the session's saved host has a login password the vault may
   * type, and the identity's name (`null` while the vault is locked);
   * `null` when it has none. Sessions without a saved host lack it.
   */
  fillOffer?(): Promise<{ identity: string | null; login: string } | null>;
  /**
   * Types that password, without Enter, if the session showed nothing
   * past the `seen` bytes of output the view has parsed (else it fails
   * as `cancelled`). The view never sees the password.
   */
  fillPassword?(seen: number): Promise<void>;
}

/** Opens terminal sessions. Components depend on this, never on Tauri. */
export interface SessionClient {
  open(spec: SessionSpec, size: GridSize, events: SessionEvents): Promise<TerminalSession>;
}

type CommandResult<T> = Promise<
  { status: "ok"; data: T } | { status: "error"; error: CommandError }
>;

async function unwrap<T>(result: CommandResult<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") {
    throw new SessionCommandError(outcome.error);
  }
  return outcome.data;
}

async function done(result: CommandResult<null>): Promise<void> {
  await unwrap(result);
}

const CANCEL: AnswerSpec = { kind: "cancel" };

export const tauriSessions: SessionClient = {
  async open(spec, size, events) {
    const output = new Channel<ArrayBuffer>();
    output.onmessage = (buffer) => events.onOutput(new Uint8Array(buffer));
    const exit = new Channel<SessionExit>();
    exit.onmessage = (event) => events.onExit(event.exitCode, event.lost);

    const prompts = () => promptChannel(events.onPrompt);
    let opening: CommandResult<number>;
    if (spec.kind === "tmux" && spec.control !== undefined && spec.pane !== undefined) {
      opening = commands.tmuxPaneAttach(spec.control, spec.pane, size, output, exit);
    } else if (spec.kind === "tmux") {
      opening = openTmux(spec.hostId, size, output, exit, prompts(), events.tmux);
    } else if (spec.kind === "local") {
      opening = commands.sessionOpen(size, output, exit);
    } else if (spec.kind === "telnet") {
      opening = commands.telnetConnect(spec.hostId, size, output, exit);
    } else if (spec.kind === "serial") {
      opening = commands.serialOpen(spec.port, output, exit);
    } else {
      opening =
        spec.kind === "mosh"
          ? commands.moshConnect(spec.hostId, size, output, exit, prompts())
          : spec.kind === "host"
            ? commands.hostConnect(spec.hostId, size, output, exit, prompts())
            : commands.sshOpen(spec.target, size, output, exit, prompts());
    }
    const id = await unwrap(opening);
    return {
      id,
      write: (data) => done(commands.sessionWrite(id, data)),
      writeBytes: (data) => done(commands.sessionWriteBytes(id, [...data])),
      resize: (next) => done(commands.sessionResize(id, next)),
      acknowledge: (bytes) => done(commands.sessionAcknowledge(id, bytes)),
      probe: () => done(commands.sessionProbe(id)),
      close: () => done(commands.sessionClose(id)),
      fillOffer: () => unwrap(commands.sessionFillOffer(id)),
      fillPassword: (seen) => done(commands.sessionFillPassword(id, seen)),
    };
  },
};

/**
 * Starts host `hostId`'s tmux control client and shows its first window's
 * pane; the other windows go to `tmux.opened`. A host whose tmux cannot
 * (`unsupported`) is joined the plain way instead.
 */
async function openTmux(
  hostId: string,
  size: GridSize,
  output: Channel<ArrayBuffer>,
  exit: Channel<SessionExit>,
  prompts: Channel<PromptRequest>,
  tmux: TmuxEvents | undefined,
): CommandResult<number> {
  const topology = new Channel<TopologyView>();
  topology.onmessage = (event) => tmux?.topology(event);
  const opened = await commands.tmuxOpen(hostId, size, topology, prompts);
  if (opened.status === "error") {
    if (opened.error.code !== "unsupported") return opened;
    tmux?.fellBack(opened.error.message);
    return commands.hostConnect(hostId, size, output, exit, prompts);
  }
  const [first, ...rest] = opened.data.windows;
  if (!first) {
    return {
      status: "error",
      error: { code: "not_found", message: "the tmux session has no window" },
    };
  }
  tmux?.opened(opened.data.control, first, rest);
  const attached = await commands.tmuxPaneAttach(
    opened.data.control,
    first.pane,
    size,
    output,
    exit,
  );
  // A client no tab shows would stay attached, holding window sizes.
  if (attached.status === "error") await commands.tmuxDetach(opened.data.control);
  return attached;
}

/** What the app asks of a tmux session shown as tabs. */
export interface TmuxClient {
  /** A new window, which comes back as a topology event. */
  newWindow(control: number): Promise<void>;
  /** Leaves the session running in tmux and stops showing it. */
  detach(control: number): Promise<void>;
}

export const tauriTmux: TmuxClient = {
  newWindow: (control) => done(commands.tmuxNewWindow(control)),
  detach: (control) => done(commands.tmuxDetach(control)),
};
