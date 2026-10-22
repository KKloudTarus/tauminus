// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { GuardFinding, GuardVerdict } from "@/ipc/index.ts";

/** Where the guard's answers come from: the saved host behind a terminal. */
export interface GuardSource {
  /** Whether commands are checked now. Follows the host's tags and settings. */
  active(): boolean;
  subscribe(listener: () => void): () => void;
  /** Checks several readings of one line in one call; `incomplete` comes from the first. */
  check(lines: string[]): Promise<GuardVerdict>;
}

/** What the user is asked to confirm. */
export interface GuardRequest {
  readonly kind: "command" | "paste";
  /** The exact text: the command line as the screen shows it, or the paste. */
  readonly text: string;
  readonly findings: readonly GuardFinding[];
}

/** One row of the screen, as xterm.js's `IBufferLine` provides it. */
export interface ScreenLine {
  readonly isWrapped: boolean;
  translateToString(trimRight?: boolean, startColumn?: number, endColumn?: number): string;
}

/** The active buffer, as xterm.js's `IBuffer` provides it. */
export interface Screen {
  readonly type: "normal" | "alternate";
  readonly cursorX: number;
  readonly cursorY: number;
  readonly baseY: number;
  /** Rows in the buffer, scrollback included. */
  readonly length: number;
  getLine(y: number): ScreenLine | undefined;
}

/** A row that follows scrolling, as xterm.js's `IMarker`; -1 once trimmed. */
export interface Marker {
  readonly line: number;
  dispose(): void;
}

export interface GuardHost {
  send(data: string): void;
  screen(): Screen;
  /** Marks the cursor's row. */
  mark(): Marker | undefined;
  /** Asks the user; true runs the command or the paste. */
  ask(request: GuardRequest): Promise<boolean>;
  /** Counts the times the terminal finished parsing output. */
  parsed(): number;
  /** Resolves true at the next parse, or false after `ms`. */
  nextParse(ms: number): Promise<boolean>;
}

/** Where the user started typing: after the prompt. */
interface Anchor {
  readonly marker: Marker | undefined;
  readonly x: number;
  readonly buffer: Screen["type"];
}

/**
 * Keys that make a shell run the line: Enter, keypad Enter, Ctrl+J,
 * Alt+Enter, and bash's Ctrl+O (operate-and-get-next).
 */
const SUBMIT = new Set(["\r", "\u001bOM", "\n", "\u001b\r", "\u000f"]);
/** Ctrl+X Ctrl+E: bash and zsh open the line in an editor and run what it saves. */
const EDIT_PREFIX = "\u0018";
const EDIT_KEY = "\u0005";
/** Escape then `v` does the same in vi editing mode. */
const VI_EDIT = "v";
/** Alt+#: bash comments the line out, or with an argument back in, and runs it. */
const COMMENT_AND_RUN = "\u001b#";
/**
 * A prompt for a secret: the keys typed after it never echo, and they are
 * not a command.
 */
const SECRET_PROMPT =
  /\b(password|passphrase|passcode|pin|token|secret|otp|verification code)\b[^:]{0,60}:\s*$/iu;
/** Most rows read for one line; past `MAX_LINE` the backend refuses it anyway. */
export const MAX_ROWS = 512;
/** How long a check may take before the user is asked instead. */
export const CHECK_TIMEOUT_MS = 3000;
/** Most texts one check sends; the backend accepts no more. */
const MAX_READINGS = 8;

const INTERRUPT = "\u0003";
const ERASE = new Set(["\u007f", "\b"]);
const KILL_LINE = "\u0015";
const KILL_WORD = "\u0017";
/**
 * How long Enter waits for the echo of the keys before it, when none has
 * arrived: on a slow link the screen lags the shell's line, and the screen
 * is what gets checked.
 */
export const ECHO_WAIT_MS = 500;
/** A gap in output this long ends a burst of echo. */
export const ECHO_QUIET_MS = 30;
const PASTE_START = "\u001b[200~";
const PASTE_END = "\u001b[201~";

/** Shown when the backend could not check a line; the user decides. */
const UNCHECKED: GuardFinding = {
  rule: "unchecked",
  reason: "could not be checked",
  command: "",
};
const EDIT: GuardFinding = {
  rule: "edit-and-run",
  reason: "opens the line in an editor, and what the editor saves runs without a check",
  command: "",
};
const COMMENTED: GuardFinding = {
  rule: "comment-and-run",
  reason: "toggles a comment on the line and runs it",
  command: "",
};
const CONTROLS: GuardFinding = {
  rule: "control-characters",
  reason: "holds control characters, such as Ctrl+C or Escape, that the shell reads as keys",
  command: "",
};

/**
 * The prod guard in the terminal: holds back a submitted line until the
 * backend has checked it, and asks the user when the check finds a reason.
 *
 * The line is read from the screen, not rebuilt from keystrokes, so history
 * recall, completion, and editing keys all count: it runs from where the
 * user started typing to the end of the cursor's wrapped line. When that
 * start is lost, the whole line is read, prompt included, and both are
 * checked. A line the shell will continue (an open quote, a trailing `|`)
 * is kept and checked again with the next one.
 *
 * The rows with text right below the cursor's line are checked too: a
 * recalled multi-line command may run on past the cursor.
 *
 * On a slow link the screen lags: Enter waits up to 500 ms for the echo of
 * the keys before it, and the printable keys typed since the prompt are
 * checked as well, so a line typed faster than it echoes is still seen.
 *
 * Input stays in order: while a check or a question is pending, later input
 * waits behind it. A multi-line paste always asks, with its findings.
 */
export class CommandGuard {
  readonly #source: GuardSource;
  readonly #host: GuardHost;
  #tail: Promise<void> = Promise.resolve();
  #queued = 0;
  #anchor: Anchor | undefined;
  /** Lines the shell is still waiting to complete. */
  #pending = "";
  /** Printable keys typed since the prompt, while only such keys and erasing were used. */
  #typed = "";
  #typedValid = true;
  /** `parsed()` when the last key went out; equal means its echo has not arrived. */
  #sentParse: number | undefined;
  #lastKey = "";
  #disposed = false;

  constructor(source: GuardSource, host: GuardHost) {
    this.#source = source;
    this.#host = host;
  }

  /** Input from the terminal: keystrokes, pastes, and terminal replies. */
  input(data: string): void {
    if (this.#disposed) return;
    if (this.#queued === 0 && !this.#source.active()) {
      this.#forget();
      this.#host.send(data);
      return;
    }
    this.#queued += 1;
    this.#tail = this.#tail
      .then(() => this.#handle(data))
      .catch((error: unknown) => console.error("prod guard failed", error))
      .finally(() => {
        this.#queued -= 1;
      });
  }

  /** Settles when every input so far has been sent or dropped. For tests. */
  settled(): Promise<void> {
    return this.#tail;
  }

  dispose(): void {
    this.#disposed = true;
    this.#forget();
  }

  async #handle(data: string): Promise<void> {
    if (this.#disposed) return;
    if (!this.#source.active()) {
      this.#forget();
      this.#host.send(data);
      return;
    }
    const edits =
      (data === EDIT_KEY && this.#lastKey === EDIT_PREFIX) ||
      (data === VI_EDIT && this.#lastKey === "\u001b") ||
      data === `\u001b${VI_EDIT}`;
    this.#lastKey = data;
    if (SUBMIT.has(data) || edits || data === COMMENT_AND_RUN) {
      const always = edits ? [EDIT] : data === COMMENT_AND_RUN ? [COMMENTED] : [];
      await this.#submit(data, always);
    } else if (pasted(unbracketed(data))) {
      await this.#paste(data);
    } else {
      if (data === INTERRUPT) {
        this.#forget();
      } else {
        this.#anchor ??= this.#mark();
        this.#type(data);
      }
      this.#sentParse = this.#host.parsed();
      this.#host.send(data);
    }
  }

  /** Follows the keys a line editor treats alike; anything else makes the copy unusable. */
  #type(data: string): void {
    if (ERASE.has(data)) {
      this.#typed = Array.from(this.#typed).slice(0, -1).join("");
    } else if (data === KILL_LINE) {
      this.#typed = "";
    } else if (data === KILL_WORD) {
      this.#typed = this.#typed.replace(/\S*\s*$/u, "");
    } else if (data !== "" && !Array.from(data).some((c) => isControl(c, true))) {
      this.#typed += data;
    } else {
      this.#typedValid = false;
    }
  }

  /** Waits for the echo of the last key, then for its burst to end. At most about 0.8 s. */
  async #settle(): Promise<void> {
    if (this.#sentParse === undefined || this.#host.parsed() !== this.#sentParse) return;
    if (!(await this.#host.nextParse(ECHO_WAIT_MS))) return;
    for (let burst = 0; burst < 10; burst += 1) {
      // oxlint-disable-next-line no-await-in-loop -- each wait depends on the one before
      if (!(await this.#host.nextParse(ECHO_QUIET_MS))) return;
    }
  }

  /** `always` are findings that ask whatever the check says. */
  async #submit(key: string, always: GuardFinding[]): Promise<void> {
    await this.#settle();
    if (this.#disposed) return;
    const { anchored, whole, below } = readLine(this.#host.screen(), this.#anchorPosition());
    // A secret typed at a prompt that hides it: only printable keys, none of
    // them shown even after the echo wait. Recalled or pasted lines, which
    // could be commands typed before the prompt redrew, are still checked.
    const secret =
      always.length === 0 &&
      anchored === "" &&
      this.#typedValid &&
      this.#typed !== "" &&
      SECRET_PROMPT.test(whole);
    if (secret) {
      this.#forget();
      this.#host.send(key);
      return;
    }
    const prefix = this.#pending ? `${this.#pending}\n` : "";
    const text = prefix + anchored;
    const typed = this.#typedValid && this.#typed.trim() !== "" ? prefix + this.#typed : "";
    // The first candidate decides whether the shell waits for more.
    // Without a start of typing, what follows each prompt ending stands in
    // for it; with one, those endings would be read out of quoted text.
    const lost = this.#anchorPosition() === undefined ? afterPrompts(whole) : [];
    const candidates = [...new Set([text, whole, below, typed, ...lost])]
      .filter((c) => c.trim() !== "")
      .slice(0, MAX_READINGS);
    const [first] = candidates;
    if (first === undefined && always.length === 0) {
      this.#forget();
      this.#host.send(key);
      return;
    }
    const verdict = await this.#check(candidates);
    if (this.#disposed) return;
    const findings = merge([...always, ...verdict.findings]);
    if (
      findings.length > 0 &&
      !(await this.#host.ask({ kind: "command", text: first ?? "", findings }))
    ) {
      // The line stays on screen for the user to edit or clear.
      return;
    }
    if (this.#disposed) return;
    const pending = first !== undefined && verdict.incomplete ? first : "";
    this.#forget();
    this.#pending = pending;
    this.#host.send(key);
  }

  async #paste(data: string): Promise<void> {
    const bracketed = data.startsWith(PASTE_START) && data.endsWith(PASTE_END);
    const inner = bracketed ? data.slice(PASTE_START.length, -PASTE_END.length) : data;
    const text = inner.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
    // A control such as Ctrl+C acts as a key: the shell drops what came
    // before it. Each piece between them is checked as its own line.
    const controls = hasControl(text);
    const all = splitAtControls(text).filter((p) => p.trim() !== "");
    // The backend reads at most `MAX_READINGS` texts; the rest go in the last.
    const pieces = [
      ...all.slice(0, MAX_READINGS - 1),
      ...(all.length >= MAX_READINGS ? [all.slice(MAX_READINGS - 1).join("\n")] : []),
    ];
    const verdict = pieces.length > 0 ? await this.#check(pieces) : undefined;
    if (this.#disposed) return;
    const findings = merge([...(controls ? [CONTROLS] : []), ...(verdict?.findings ?? [])]);
    if (await this.#host.ask({ kind: "paste", text, findings })) {
      if (this.#disposed) return;
      // Where typing started still counts: the next Enter reads from there,
      // not from the whole line, whose prompt could hide the command.
      this.#anchor ??= this.#mark();
      this.#typedValid = false;
      this.#sentParse = this.#host.parsed();
      this.#host.send(data);
    }
  }

  async #check(lines: string[]): Promise<GuardVerdict> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error("the check timed out")), CHECK_TIMEOUT_MS);
    });
    try {
      return await Promise.race([this.#source.check(lines), timeout]);
    } catch (error) {
      console.error("prod guard check failed", error);
      return {
        findings: [{ ...UNCHECKED, command: (lines[0] ?? "").slice(0, 200) }],
        incomplete: false,
      };
    } finally {
      clearTimeout(timer);
    }
  }

  #mark(): Anchor {
    const screen = this.#host.screen();
    return { marker: this.#host.mark(), x: screen.cursorX, buffer: screen.type };
  }

  #anchorPosition(): { line: number; x: number } | undefined {
    const anchor = this.#anchor;
    if (!anchor?.marker || anchor.marker.line < 0) return undefined;
    if (anchor.buffer !== this.#host.screen().type) return undefined;
    return { line: anchor.marker.line, x: anchor.x };
  }

  #forget(): void {
    this.#lastKey = "";
    this.#anchor?.marker?.dispose();
    this.#anchor = undefined;
    this.#pending = "";
    this.#typed = "";
    this.#typedValid = true;
  }
}

/**
 * The cursor's line, unwrapped: from `anchor` when it lies on that line and
 * before the cursor (`anchored`), and whole (`whole`). `below` runs from the
 * same start through the rows with text right after the line, one line per
 * unwrapped line: a recalled multi-line command shows there.
 * O(r·c) for r ≤ `MAX_ROWS` rows of c columns.
 */
export function readLine(
  screen: Screen,
  anchor: { line: number; x: number } | undefined,
): { anchored: string; whole: string; below: string } {
  const row = screen.baseY + screen.cursorY;
  let start = row;
  while (start > 0 && row - start < MAX_ROWS && screen.getLine(start)?.isWrapped) start -= 1;
  let end = row;
  while (end - start < MAX_ROWS && screen.getLine(end + 1)?.isWrapped) end += 1;
  let last = end;
  while (
    last + 1 < screen.length &&
    last - start < MAX_ROWS &&
    (screen.getLine(last + 1)?.translateToString(true) ?? "") !== ""
  ) {
    last += 1;
  }
  const read = (fromRow: number, fromX: number, toRow: number) => {
    let text = "";
    for (let y = fromRow; y <= toRow; y += 1) {
      const line = screen.getLine(y);
      if (!line) continue;
      if (y > fromRow && !line.isWrapped) text = `${text.trimEnd()}\n`;
      // Wrapped rows keep their trailing spaces: the text runs on.
      const next = screen.getLine(y + 1);
      text += line.translateToString(!next?.isWrapped, y === fromRow ? fromX : 0);
    }
    return text.trimEnd();
  };
  const whole = read(start, 0, end);
  const usable =
    anchor !== undefined &&
    anchor.line >= start &&
    anchor.line <= end &&
    (anchor.line < row || (anchor.line === row && anchor.x <= screen.cursorX));
  const from = usable ? { row: anchor.line, x: anchor.x } : { row: start, x: 0 };
  return {
    anchored: usable ? read(anchor.line, anchor.x, end) : whole,
    whole,
    // Full-screen programs fill the rows below with their own text.
    below: last > end && screen.type === "normal" ? read(from.row, from.x, last) : "",
  };
}

/** Findings without repeats, in the order the backend gave them. */
function merge(findings: readonly GuardFinding[]): GuardFinding[] {
  const seen = new Set<string>();
  return findings.filter((f) => {
    const key = `${f.rule}\u0000${f.command}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/**
 * A C0 control or DEL. Tab and newline count only when `all` is set: a
 * pasted line keeps them as text, and a line editor reads the rest as keys.
 */
function isControl(char: string, all = false): boolean {
  const code = char.codePointAt(0) ?? 0;
  if (!all && (char === "\t" || char === "\n")) return false;
  return code < 0x20 || code === 0x7f;
}

function hasControl(text: string): boolean {
  return Array.from(text).some((c) => isControl(c));
}

/**
 * One escape sequence, as a key or a terminal reply sends it: ESC and
 * printable characters (CSI, SS3), or OSC, DCS, APC, PM, and SOS with a
 * printable body up to BEL or ST.
 */
function isSequence(data: string): boolean {
  if (!data.startsWith("\u001b")) return false;
  // Alt with any key, including Alt+Backspace and Ctrl+Alt+letter.
  if (Array.from(data).length === 2) return true;
  const body = data.slice(1);
  if (!hasControl(body)) return true;
  const end = body.endsWith("\u0007") ? 1 : body.endsWith("\u001b\\") ? 2 : 0;
  return end > 0 && "]P_^X".includes(body.charAt(0)) && !hasControl(body.slice(0, -end));
}

/**
 * Input that did not come from one key: a paste, or a chunk the shell would
 * read as several keys. A newline runs a line; any other control would act
 * as an editing key.
 */
function pasted(data: string): boolean {
  if (data.includes("\r") || data.includes("\n")) return true;
  return data.length > 1 && hasControl(data) && !isSequence(data);
}

/** The text of a bracketed paste, or `data` itself. */
function unbracketed(data: string): string {
  return data.startsWith(PASTE_START) && data.endsWith(PASTE_END)
    ? data.slice(PASTE_START.length, -PASTE_END.length)
    : data;
}

/** The text between controls other than tab and newline. */
function splitAtControls(text: string): string[] {
  const pieces = [""];
  for (const char of text) {
    if (isControl(char)) {
      pieces.push("");
    } else {
      pieces[pieces.length - 1] += char;
    }
  }
  return pieces;
}

/** Endings of common prompts: `# ` for root, `$ `, `% ` for zsh, `> `. */
const PROMPT_ENDS = ["# ", "$ ", "% ", "> "];
/** Most prompt endings read on one line. */
const MAX_PROMPT_ENDS = 4;

/**
 * What follows each prompt-like ending on the line, for when the start of
 * typing is lost: read whole, `/ # rm -rf /srv` is a comment and
 * `prod> rm -rf /srv` a redirect, while the shell runs `rm -rf /srv`.
 */
export function afterPrompts(line: string): string[] {
  const found: string[] = [];
  // From the right: the prompt's own ending is the last one before the
  // command, however many come earlier.
  for (let at = line.length - 1; at >= 0 && found.length < MAX_PROMPT_ENDS; at -= 1) {
    const end = PROMPT_ENDS.find((p) => line.startsWith(p, at));
    if (end) found.push(line.slice(at + end.length));
  }
  return found;
}
