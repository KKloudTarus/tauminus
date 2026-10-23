// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type Marker,
  type Screen,
  SECRET_PROMPT,
  SUBMIT,
  afterPrompts,
  readLine,
} from "./command-guard.ts";

/** One command as it ran, for the history. */
export interface RecordedCommand {
  command: string;
  output: string;
  exit: number | null;
  /** When it was run, in milliseconds since the Unix epoch. */
  startedMs: number;
}

export interface RecorderHost {
  screen(): Screen;
  /** Marks the cursor's row. */
  mark(): Marker | undefined;
  record(entry: RecordedCommand): void;
  now(): number;
}

/** Most output rows kept per command. */
export const MAX_OUTPUT_ROWS = 2000;
/**
 * Most output characters sent per command: the backend keeps 64 KiB, and
 * reads 4 KiB past it to hide a secret the cut would end short.
 */
export const MAX_OUTPUT_CHARS = 68 * 1024;
/** Printable keys kept to tell whether they echoed: the last word's worth. */
const MAX_TYPED = 256;

const PASTE_START = "\u001b[200~";
const PASTE_END = "\u001b[201~";
const INTERRUPT = "\u0003";
const KILL_LINE = "\u0015";
const KILL_WORD = "\u0017";
const ERASE = new Set(["\u007f", "\b"]);
/** bash and readline's incremental search, as it shows on the line. */
const SEARCH = /\((?:reverse|bck|fwd)-i-search\)`[^']*': ?(.*)$/u;
/** zsh's right prompt, or any text far to the right of the command. */
const RIGHT_PROMPT = /\s{4,}\S(?:.*\S)?$/u;
/** The last character of a shell's prompt: `$`, `#`, `%`, `>`, and common themes. */
const PROMPT_END = /[$#%>❯➜λ»]\s*$/u;
/**
 * Prompts of programs that are not shells: Python, IPython, irb, pry, SQL
 * clients, Redis, Mongo, Scala, GHCi, Node. Lines typed there are not
 * commands.
 */
const REPL_PROMPT =
  /^(?:>>>|\.\.\.|In \[\d+\]:|irb[\w.:()-]*[>*]|\[\d+\] pry\(|mysql>|MariaDB \[|sqlite>|\w+[=-][#>]|redis[\w.:-]*>|mongo\w*>|test>|scala>|ghci>|s?ftp>|[\w.-]+:\d+>|λ)$/u;
/**
 * Replies the terminal sends on the program's behalf: OSC and DCS answers
 * (colours, clipboard), focus in and out, cursor position, device
 * attributes. They are not keys, and they mark no prompt.
 */
const REPLY =
  // oxlint-disable-next-line no-control-regex -- terminal replies are control sequences
  /^(?:\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\u001bP[^\u001b]*\u001b\\|\u001b\[[IO]|\u001b\[\d+;\d+R|\u001b\[[?>]?[\d;]*c)$/u;
/** The start of a secret's prompt on a line, to cut what follows it. */
const SECRET_START =
  /(?:\b(?:password|passphrase|pass phrase|passcode|passwort|pin|token|secret|otp|one-time code|verification code|authenticator code|api key|access key|secret key|private key|credential)s?\b|mot de passe|contraseña|senha|mật khẩu|パスワード|密码)/iu;

interface Cut {
  /** Where the typed text starts: the anchor's row and column. */
  from: Marker | undefined;
  x: number;
  /** The row it ends on. */
  to: Marker | undefined;
}

interface Running {
  command: string;
  /** The command's last row: output starts on the row after it. */
  marker: Marker | undefined;
  rowsAfter: number;
  exit: number | null;
  startedMs: number;
  /** Where the shell said the output starts (OSC 133;C), when it says. */
  outputMarker: Marker | undefined;
  /** Text typed into the running program, kept out of its output. */
  cuts: Cut[];
}

/**
 * Records the commands a shell runs, read from the screen as the guard
 * reads them: on Enter, the line from where the user started typing at the
 * prompt. A command's output is the text between its line and the next
 * prompt, so it is recorded when the next command starts, or when the
 * session ends. With shell integration (OSC 133) the shell marks the
 * prompt, the command, and the output, and only its marks start a command.
 *
 * Feed it what reaches the shell: behind the prod guard, only what the
 * guard let through, so a refused command is never recorded.
 *
 * A line is a command only after a shell's prompt. Anything typed into a
 * running program (an answer, a password, stdin) is not, and is cut from
 * that program's output, which keeps its prompt only. Nothing is recorded
 * when the line starts with a space (bash's `ignorespace`), in a
 * full-screen program, or at a REPL's prompt.
 */
export class CommandRecorder {
  readonly #host: RecorderHost;
  /** Where the line editor starts at this prompt. */
  #anchor: { marker: Marker | undefined; x: number } | undefined;
  /** The last printable keys typed since then, to tell whether they echoed. */
  #typed = "";
  /** Text pasted since then, which is the command when it has lines. */
  #pasted = "";
  #running: Running | undefined;
  /** The shell marks prompts (OSC 133): only its marks start commands. */
  #integrated = false;
  /** Between the shell's B and C marks: the line being typed is a command. */
  #atPrompt = false;
  /** The prompt the last command was typed at, which the shell shows again. */
  #lastPrompt: string | undefined;
  #disposed = false;

  constructor(host: RecorderHost) {
    this.#host = host;
  }

  /** What was sent to the shell, chunk by chunk, in order. */
  input(data: string): void {
    if (this.#disposed || REPLY.test(data)) return;
    const screen = this.#host.screen();
    if (screen.type === "alternate") {
      this.#forget();
      return;
    }
    if (SUBMIT.has(data)) {
      this.#submit(screen);
      return;
    }
    if (data === INTERRUPT) {
      // At a prompt, Ctrl+C drops the line, and the command before it is
      // done; while a command runs, it stops it, and its output goes on.
      if (this.#atShellPrompt(screen)) this.#finish(screen, lineStart(screen));
      this.#forget();
      return;
    }
    if (!this.#anchor) {
      // The first key at a prompt, whatever it is (a letter, Up, Ctrl+R):
      // the line editor starts where the cursor is now.
      this.#anchor = { marker: this.#host.mark(), x: screen.cursorX };
    }
    if (data === KILL_LINE) {
      this.#typed = "";
    } else if (ERASE.has(data)) {
      this.#typed = dropLast(this.#typed);
    } else if (data === KILL_WORD) {
      this.#typed = dropWord(this.#typed);
    } else if (data.startsWith(PASTE_START) && data.endsWith(PASTE_END)) {
      const text = data.slice(PASTE_START.length, -PASTE_END.length);
      this.#pasted = (this.#pasted + text).slice(-MAX_OUTPUT_CHARS);
      this.#type(text.split(/\r\n?|\n/u).at(-1) ?? "");
    } else if (/[\r\n]/u.test(data) && data.length > 1) {
      this.#unbracketed(data, screen);
    } else if (!data.startsWith("\u001b")) {
      this.#type(data);
    }
  }

  /**
   * Whether the shell waits for a command, as its marks (OSC 133) say;
   * `undefined` for a shell that does not mark its prompts.
   */
  get atPrompt(): boolean | undefined {
    return this.#integrated ? this.#atPrompt : undefined;
  }

  /** Shell integration, OSC 133: `A` prompt, `B` input, `C` output, `D;n` done. */
  shellMark(data: string): void {
    if (this.#disposed) return;
    const screen = this.#host.screen();
    switch (data.charAt(0)) {
      case "A":
        // A alone marks no input: the screen is read as without marks
        // until a B says where input starts.
        // A secondary prompt (kitty's `k=s`): the command goes on.
        if (/;k=s(?:;|$)/u.test(data)) {
          this.#atPrompt = true;
          this.#forget();
          break;
        }
        this.#atPrompt = false;
        // A new prompt: what ran before has finished printing.
        if (screen.type === "normal") this.#finish(screen, lineStart(screen));
        this.#forget();
        break;
      case "B":
        this.#integrated = true;
        this.#atPrompt = true;
        this.#forget();
        this.#anchor = { marker: this.#host.mark(), x: screen.cursorX };
        break;
      case "C":
        this.#atPrompt = false;
        if (this.#running && !this.#running.outputMarker) {
          this.#running.outputMarker = this.#host.mark();
        }
        break;
      case "D": {
        // C and D alone prove no prompt marks: only A and B switch over.
        const code = /^D;(-?\d{1,9})/u.exec(data)?.[1];
        if (this.#running && code !== undefined) this.#running.exit = Number(code);
        break;
      }
      default:
        break;
    }
  }

  /** The session ended: the running command's output runs to the cursor's line. */
  flush(): void {
    if (this.#disposed) return;
    const screen = this.#host.screen();
    if (screen.type === "normal") {
      this.#finish(screen, lineStart(screen));
    } else {
      this.#drop(this.#running);
    }
    this.#running = undefined;
  }

  dispose(): void {
    this.#disposed = true;
    this.#forget();
    this.#drop(this.#running);
    this.#running = undefined;
  }

  /** For tests: whether the shell's own marks are in use. */
  get integrated(): boolean {
    return this.#integrated;
  }

  #type(text: string): void {
    // Only the tail is kept: a long paste is not walked whole. O(MAX_TYPED).
    this.#typed = (this.#typed + printable(text.slice(-2 * MAX_TYPED))).slice(-MAX_TYPED);
  }

  /** The anchor, when it lies on the cursor's line before the cursor. */
  #usableAnchor(screen: Screen): { line: number; x: number } | undefined {
    const anchor = this.#anchor;
    const line = anchor?.marker?.line ?? -1;
    if (!anchor || line < 0) return undefined;
    const row = screen.baseY + screen.cursorY;
    const usable =
      line >= lineStart(screen) &&
      line <= lineEnd(screen) &&
      (line < row || (line === row && anchor.x <= screen.cursorX));
    return usable ? { line, x: anchor.x } : undefined;
  }

  /** Whether the cursor's line is a shell's prompt waiting for a command. */
  #atShellPrompt(screen: Screen): boolean {
    if (this.#integrated) return this.#atPrompt;
    const at = this.#usableAnchor(screen);
    return isShellPrompt(promptOf(readLine(screen, at), at !== undefined));
  }

  #submit(screen: Screen): void {
    const at = this.#usableAnchor(screen);
    const line = readLine(screen, at);
    const typed = this.#typed;
    const pasted = this.#pasted;
    const anchor = this.#anchor;
    this.#anchor = undefined;
    this.#typed = "";
    this.#pasted = "";
    // A paste with lines ends on the shell's PS2: its prompt is on the row
    // the paste started at.
    const pastedRow =
      /[\r\n]/u.test(pasted.trim()) && anchor?.marker && anchor.marker.line >= 0
        ? screen.getLine(anchor.marker.line)?.translateToString(true, 0, anchor.x)
        : undefined;
    const prompt = pastedRow ?? promptOf(line, at !== undefined);
    const promptRow = lineStart(screen);
    const running = this.#running;
    // A line that goes on with the command just typed: the shell's PS2
    // (`> `) with nothing printed since, or with shell marks, a line typed
    // before the command's output started.
    // With shell marks, the shell says so with a B (or kitty's `A;k=s`)
    // since the Enter; a REPL's prompt never goes on with a command.
    const continuation =
      running !== undefined &&
      !running.outputMarker &&
      !REPL_PROMPT.test(prompt.trim()) &&
      ((this.#integrated && this.#atPrompt) ||
        (prompt.trim() === ">" && outputStart(running) === promptRow));
    // bash's reverse search shows its own text in place of the prompt.
    const searching = SEARCH.test(line.whole);
    const promptLike =
      continuation ||
      (this.#integrated
        ? this.#atPrompt
        : searching || (isShellPrompt(prompt) && this.#samePrompt(prompt)));
    this.#atPrompt = false;
    const secret = SECRET_PROMPT.test(prompt) || SECRET_PROMPT.test(line.whole);
    const unechoed = typed.trim() !== "" && !echoed(typed, line.whole);
    if (!promptLike || secret || unechoed) {
      // Typed into a running program, or a secret: not a command, and the
      // program keeps running. What was typed is cut from its output.
      this.#cut(anchor, line.whole);
      return;
    }
    const read = this.#command(line, at !== undefined, pasted);
    if (continuation && running && read !== undefined) {
      running.command += `\n${read}`;
      running.marker?.dispose();
      running.marker = this.#host.mark();
      running.rowsAfter = lineEnd(screen) - (screen.baseY + screen.cursorY);
      anchor?.marker?.dispose();
      return;
    }
    anchor?.marker?.dispose();
    this.#finish(screen, promptRow);
    if (read === undefined || REPL_PROMPT.test(prompt.trim())) return;
    this.#lastPrompt = prompt;
    this.#start(read, screen);
  }

  /**
   * Whether `prompt` may be the shell's own, while a command runs: it ends
   * as the last command's prompt did. A program's prompt (`Enter key:`,
   * `Continue?`) ends differently. `>` ends REPLs' prompts and fish's
   * alike, so there the start must match too; fish 4 marks its prompts
   * (OSC 133), which settles it.
   */
  #samePrompt(prompt: string): boolean {
    const last = this.#lastPrompt?.trim();
    if (this.#running === undefined || last === undefined) return true;
    const now = prompt.trim();
    if (now.slice(-1) !== last.slice(-1)) return false;
    return now.slice(-1) !== ">" || now.slice(0, 4) === last.slice(0, 4);
  }

  /** Keeps what was just typed out of the running program's output. */
  #cut(anchor: { marker: Marker | undefined; x: number } | undefined, whole: string): void {
    const running = this.#running;
    const from = anchor?.marker;
    if (!running) {
      from?.dispose();
      return;
    }
    const known = from !== undefined && from.line >= 0;
    running.cuts.push({
      from: known ? from : this.#host.mark(),
      x: known ? (anchor?.x ?? 0) : secretStart(whole),
      to: this.#host.mark(),
    });
  }

  /**
   * The command on the line: the paste when it had lines, else the text
   * from the anchor, else what follows the last prompt-like ending. A line
   * that starts with a space, or holds none of these, is not recorded.
   */
  #command(
    line: ReturnType<typeof readLine>,
    anchored: boolean,
    pasted: string,
  ): string | undefined {
    if (/[\r\n]/u.test(pasted.trim())) return pasted.trim();
    const searched = SEARCH.exec(line.whole)?.[1];
    if (searched !== undefined) return clean(searched) || undefined;
    let text: string;
    if (anchored) {
      text = line.anchored;
    } else {
      const [last] = afterPrompts(line.whole);
      if (last === undefined) return undefined;
      text = last;
    }
    if (text.startsWith(" ")) return undefined;
    const command = clean(text);
    return command === "" ? undefined : command;
  }

  #start(command: string, screen: Screen): void {
    this.#running = {
      command,
      marker: this.#host.mark(),
      rowsAfter: lineEnd(screen) - (screen.baseY + screen.cursorY),
      exit: null,
      startedMs: this.#host.now(),
      outputMarker: undefined,
      cuts: [],
    };
  }

  /**
   * A chunk with line endings and no paste brackets: a paste into a shell
   * without bracketed paste, which runs each line. Each line after a shell
   * prompt is a command; the last one's output holds what the earlier ones
   * printed too. A line after a prompt for a secret, or in a program, is
   * not.
   */
  #unbracketed(data: string, screen: Screen): void {
    const lines = data.split(/\r\n?|\n/u);
    const rest = lines.pop() ?? "";
    const at = this.#usableAnchor(screen);
    const line = readLine(screen, at);
    const prompt = promptOf(line, at !== undefined);
    const anchor = this.#anchor;
    this.#anchor = undefined;
    this.#typed = "";
    this.#pasted = "";
    const promptLike = this.#integrated
      ? this.#atPrompt
      : isShellPrompt(prompt) && this.#samePrompt(prompt);
    if (!promptLike || SECRET_PROMPT.test(prompt) || SECRET_PROMPT.test(line.whole)) {
      this.#cut(anchor, line.whole);
      return;
    }
    anchor?.marker?.dispose();
    this.#lastPrompt = prompt;
    const before = at ? line.anchored : "";
    this.#finish(screen, lineStart(screen));
    lines.forEach((text, n) => {
      const raw = n === 0 ? before + text : text;
      const command = clean(raw);
      if (command === "" || raw.startsWith(" ")) return;
      this.#finishNow();
      this.#start(command, screen);
    });
    if (rest !== "") {
      this.#anchor = { marker: this.#host.mark(), x: screen.cursorX };
      this.#type(rest);
    }
  }

  /** Records the running command with no output: more lines followed it at once. */
  #finishNow(): void {
    const running = this.#running;
    if (!running) return;
    this.#running = undefined;
    this.#drop(running);
    this.#host.record({
      command: running.command,
      output: "",
      exit: running.exit,
      startedMs: running.startedMs,
    });
  }

  /** Records the running command with the rows before `untilRow` as output. */
  #finish(screen: Screen, untilRow: number): void {
    const running = this.#running;
    if (!running) return;
    this.#running = undefined;
    const from = outputStart(running);
    const cuts = running.cuts.map((cut) => ({
      from: cut.from?.line ?? -1,
      x: cut.x,
      to: cut.to?.line ?? -1,
    }));
    this.#drop(running);
    const to = Math.min(untilRow, from + MAX_OUTPUT_ROWS, screen.length);
    const rows: string[] = [];
    let size = 0;
    // Inside a cut: its rows, and every row wrapped onto them (after a
    // resize, or with the cursor short of the end at Enter), go.
    let cutting = false;
    for (let y = from; y < to && size < MAX_OUTPUT_CHARS; y += 1) {
      const row = screen.getLine(y);
      if (!row) continue;
      // A cut whose first row left the buffer covers the rows from the top.
      const cut = cuts.find((c) => {
        const start = c.from >= 0 ? c.from : c.to >= 0 ? 0 : -1;
        return start >= 0 && y >= start && y <= Math.max(start, c.to);
      });
      if (y === from && from === 0 && cut === undefined && row.isWrapped) {
        // The command's line left the buffer: a row wrapped from above may
        // be the rest of something cut.
        cutting = true;
      }
      if (cut === undefined && cutting && row.isWrapped) continue;
      cutting = cut !== undefined;
      if (cut !== undefined && y !== cut.from) continue;
      // On a cut's first row the program's prompt stays and what was typed
      // after it does not; the rest of the cut's rows go.
      const text =
        cut === undefined
          ? row.translateToString(true)
          : `${row.translateToString(true, 0, cut.x).trimEnd()} •••`.trimStart();
      size += text.length;
      if (row.isWrapped && rows.length > 0 && cut === undefined) rows[rows.length - 1] += text;
      else rows.push(text);
    }
    this.#host.record({
      command: running.command,
      output: rows.join("\n").trimEnd().slice(0, MAX_OUTPUT_CHARS),
      exit: running.exit,
      startedMs: running.startedMs,
    });
  }

  /** Frees a command's markers. */
  #drop(running: Running | undefined): void {
    if (!running) return;
    running.marker?.dispose();
    running.outputMarker?.dispose();
    for (const cut of running.cuts) {
      cut.from?.dispose();
      cut.to?.dispose();
    }
  }

  #forget(): void {
    this.#anchor?.marker?.dispose();
    this.#anchor = undefined;
    this.#typed = "";
    this.#pasted = "";
  }
}

/**
 * The row the running command's output starts on: where the shell said
 * (OSC 133;C), or the row after the command's line. A command whose own
 * line scrolled out of the buffer starts at the oldest row kept.
 */
function outputStart(running: Running): number {
  const said = running.outputMarker?.line ?? -1;
  if (said >= 0) return said;
  const marked = running.marker && running.marker.line >= 0 ? running.marker.line : -1;
  return marked < 0 ? 0 : marked + running.rowsAfter + 1;
}

/** Whether `prompt` ends as a shell's does, and is no REPL's. */
function isShellPrompt(prompt: string): boolean {
  const trimmed = prompt.trim();
  // A bare `>` is the shell's PS2, or a REPL's (Node, Deno, R): never a new command.
  if (trimmed === ">") return false;
  const last = trimmed.split(/\s+/u).at(-1) ?? "";
  return PROMPT_END.test(trimmed) && !REPL_PROMPT.test(last);
}

/** The text before the command on the line: the prompt. */
function promptOf(line: ReturnType<typeof readLine>, anchored: boolean): string {
  if (anchored) return line.whole.slice(0, Math.max(0, line.whole.length - line.anchored.length));
  const [last] = afterPrompts(line.whole);
  return last === undefined ? line.whole : line.whole.slice(0, line.whole.length - last.length);
}

/** Where a secret starts on a line read whole: after its prompt's ending. */
function secretStart(line: string): number {
  const start = line.search(SECRET_START);
  if (start < 0) return 0;
  const match = /[:>?]\s*/u.exec(line.slice(start));
  return match ? start + match.index + match[0].length : 0;
}

/** Whether the last word typed shows on the line, as the echo would show it. */
function echoed(typed: string, line: string): boolean {
  const words = typed.trim().split(/\s+/u);
  const last = (words[words.length - 1] ?? "").toLowerCase();
  // Completion may change case or finish the word: compare its start.
  return line.toLowerCase().includes(last.slice(0, Math.max(1, Math.min(last.length, 3))));
}

/** The command without a right prompt or surrounding space. */
function clean(text: string): string {
  return text.replace(RIGHT_PROMPT, "").trim();
}

function printable(text: string): string {
  let out = "";
  for (const c of text) {
    const code = c.codePointAt(0) ?? 0;
    if (code >= 0x20 && code !== 0x7f) out += c;
  }
  return out;
}

/** Without its last character, a surrogate pair counted as one. */
function dropLast(text: string): string {
  if (text === "") return text;
  const code = text.charCodeAt(text.length - 1);
  return text.slice(0, code >= 0xdc00 && code <= 0xdfff ? -2 : -1);
}

/** Without its last word and the spaces after it, as Ctrl+W does. O(word). */
function dropWord(text: string): string {
  let end = text.length;
  while (end > 0 && /\s/u.test(text.charAt(end - 1))) end -= 1;
  while (end > 0 && !/\s/u.test(text.charAt(end - 1))) end -= 1;
  return text.slice(0, end);
}

/** The first row of the cursor's wrapped line. */
function lineStart(screen: Screen): number {
  let row = screen.baseY + screen.cursorY;
  while (row > 0 && screen.getLine(row)?.isWrapped) row -= 1;
  return row;
}

/** The last row of the cursor's wrapped line. */
function lineEnd(screen: Screen): number {
  let row = screen.baseY + screen.cursorY;
  while (screen.getLine(row + 1)?.isWrapped) row += 1;
  return row;
}
