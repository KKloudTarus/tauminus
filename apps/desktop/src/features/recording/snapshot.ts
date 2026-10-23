// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * A terminal's state as text that, written into a fresh terminal of the
 * same size, makes it behave the same from then on: the seek checkpoints
 * of the player.
 *
 * `@xterm/addon-serialize` writes the screen, scrollback, and most modes,
 * but not the scroll region, tab stops, the saved cursor, the character
 * sets, a hidden cursor, the cursor's exact place (pending wrap, origin
 * mode), or the current attributes; replay from such a checkpoint drifts
 * in full-screen programs. The suffix here restores those, read from
 * xterm.js 6.0.0's internals: pinned, and checked by a differential test.
 * A checkpoint is taken only between escape sequences.
 */

/**
 * What a checkpoint keeps of scrollback. A checkpoint's size is
 * scrollback × columns × the colour in each cell, so this bounds what
 * seeks cost and what checkpoints weigh. The terminals that make
 * checkpoints keep exactly this much, so a snapshot holds all of theirs.
 */
export const CHECKPOINT_SCROLLBACK = 200;

/** Names the version of this format, xterm.js, and its Unicode tables. */
export const SNAPSHOT_FORMAT = "tauminus-snapshot/2 xterm-6.0.0 unicode-11 sb-200";

/** The serialize addon, as far as this uses it. */
export interface Serializer {
  serialize(options?: { scrollback?: number }): string;
}

/** A terminal, as far as this uses it: the public part and the internals. */
export interface SnapshotTerminal {
  readonly cols: number;
  readonly rows: number;
}

/** Reads `obj[name]` without trusting it has that shape. */
function get(obj: unknown, name: string): unknown {
  return typeof obj === "object" && obj !== null ? Reflect.get(obj, name) : undefined;
}

function num(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function call(obj: unknown, name: string, ...args: unknown[]): unknown {
  const fn = get(obj, name);
  return typeof fn === "function" ? Reflect.apply(fn, obj, args) : undefined;
}

/** The character set a table is: DEC line drawing, UK, or plain ASCII. */
function designator(charset: unknown): string {
  if (typeof charset !== "object" || charset === null) return "B";
  if (get(charset, "q") === "─") return "0";
  if (get(charset, "#") === "£") return "A";
  return "B";
}

/** SGR that sets exactly `attr`: colours, flags, and underline style. */
function sgr(attr: unknown): string {
  const params: string[] = ["0"];
  const fg = num(get(attr, "fg"));
  const bg = num(get(attr, "bg"));
  const colour = (value: number, base: number, bright: number, extended: number) => {
    const mode = value & 0x3000000;
    const code = value & 0xffffff;
    if (mode === 0x1000000 || mode === 0x2000000) {
      if (code >= 16) params.push(`${extended};5;${code}`);
      else params.push(String(code & 8 ? bright + (code & 7) : base + (code & 7)));
    } else if (mode === 0x3000000) {
      params.push(`${extended};2;${(code >> 16) & 255};${(code >> 8) & 255};${code & 255}`);
    }
  };
  colour(fg, 30, 90, 38);
  colour(bg, 40, 100, 48);
  const flag = (method: string, code: string) => {
    if (call(attr, method)) params.push(code);
  };
  flag("isBold", "1");
  flag("isDim", "2");
  flag("isItalic", "3");
  if (call(attr, "isUnderline")) {
    const style = num(call(attr, "getUnderlineStyle"));
    params.push(style > 0 ? `4:${style}` : "4");
  }
  flag("isBlink", "5");
  flag("isInverse", "7");
  flag("isInvisible", "8");
  flag("isStrikethrough", "9");
  flag("isOverline", "53");
  // Underline colour, from the extended attributes.
  const extended = get(attr, "extended");
  const underlineColour = num(get(extended, "underlineColor"));
  const ulMode = underlineColour & 0x3000000;
  const ul = underlineColour & 0xffffff;
  if (ulMode === 0x3000000) params.push(`58;2;${(ul >> 16) & 255};${(ul >> 8) & 255};${ul & 255}`);
  else if (ulMode !== 0) params.push(`58;5;${ul}`);
  return `\u001b[${params.join(";")}m`;
}

/**
 * The parser is between sequences: a checkpoint here cuts nothing. A
 * terminal whose internals are not as expected is never "at rest", so no
 * checkpoint is taken rather than a wrong one.
 */
export function atRest(term: SnapshotTerminal): boolean {
  const parser = get(get(get(term, "_core"), "_inputHandler"), "_parser");
  return get(parser, "currentState") === 0;
}

/**
 * `term`'s state as text, or `undefined` when its parser is mid-sequence
 * or its internals are not xterm.js 6.0.0's. O(scrollback × cols).
 */
export function snapshot(
  term: SnapshotTerminal,
  serializer: Serializer,
  scrollback = CHECKPOINT_SCROLLBACK,
  /** Snapshot even mid-sequence: what is left of the sequence is lost. */
  force = false,
): string | undefined {
  const core = get(term, "_core");
  const buffer = get(core, "buffer");
  const charsets = get(core, "_charsetService");
  const service = get(core, "coreService");
  const input = get(core, "_inputHandler");
  if (
    (!force && !atRest(term)) ||
    buffer === undefined ||
    charsets === undefined ||
    service === undefined
  ) {
    return undefined;
  }
  const modes = get(service, "decPrivateModes");
  let out = serializer.serialize({ scrollback });
  // Tab stops.
  out += "\u001b[3g";
  const tabs = get(buffer, "tabs");
  if (typeof tabs === "object" && tabs !== null) {
    for (const [column, set] of Object.entries(tabs)) {
      if (set) out += `\u001b[1;${Number(column) + 1}H\u001bH`;
    }
  }
  // The saved cursor (DECSC): place, attributes, and character set. Its row
  // is counted from the top of the buffer.
  const ybase = num(get(buffer, "ybase"));
  const savedY = Math.max(0, num(get(buffer, "savedY")) - ybase);
  const savedX = num(get(buffer, "savedX"));
  out += `\u001b[${savedY + 1};${savedX + 1}H${sgr(get(buffer, "savedCurAttrData"))}`;
  out += `\u001b(${designator(get(buffer, "savedCharset"))}\u000f\u001b7`;
  // The scroll region homes the cursor, so origin mode goes after it.
  const top = num(get(buffer, "scrollTop"));
  const bottom = num(get(buffer, "scrollBottom"));
  out += `\u001b[${top + 1};${bottom + 1}r`;
  const origin = get(modes, "origin") === true;
  if (origin) out += "\u001b[?6h";
  // The cursor. Past the last column it waits to wrap: write the last cell
  // again, with its own attributes, so the next character wraps too.
  const x = num(get(buffer, "x"));
  const y = num(get(buffer, "y"));
  const row = y - (origin ? top : 0) + 1;
  if (x >= term.cols) {
    // The last cell may be the second half of a wide character: write the
    // character from where it starts.
    const last = publicCell(term, ybase + y, term.cols - 1);
    const wide = last.width === 0 && term.cols >= 2;
    const cell = wide ? publicCell(term, ybase + y, term.cols - 2) : last;
    const column = wide ? term.cols - 1 : term.cols;
    const insert =
      get(modes, "insertMode") === true || get(get(term, "modes"), "insertMode") === true;
    out += `\u001b[${row};${column}H${cell.sgr}${insert ? "\u001b[4l" : ""}${cell.chars || " "}`;
    if (insert) out += "\u001b[4h";
  } else {
    out += `\u001b[${row};${x + 1}H`;
  }
  // The current attributes and character sets.
  out += sgr(get(input, "_curAttrData"));
  const sets = get(charsets, "_charsets");
  for (let g = 0; g < 4; g += 1) out += `\u001b${"()*+"[g]}${designator(get(sets, String(g)))}`;
  // The set in use: SI/SO for G0/G1, LS2/LS3 for G2/G3.
  const glevel = num(get(charsets, "glevel"));
  out += ["\u000f", "\u000e", "\u001bn", "\u001bo"][glevel] ?? "\u000f";
  if (get(service, "isCursorHidden") === true) out += "\u001b[?25l";
  return out;
}

/** A cell's text and the SGR that draws it, read through the public API. */
function publicCell(
  term: SnapshotTerminal,
  row: number,
  column: number,
): { chars: string; sgr: string; width: number } {
  const cell = call(call(get(get(term, "buffer"), "active"), "getLine", row), "getCell", column);
  if (cell === undefined) return { chars: "", sgr: "\u001b[0m", width: 1 };
  const params: string[] = ["0"];
  const colour = (prefix: "Fg" | "Bg", base: number, bright: number, extended: number) => {
    const value = num(call(cell, `get${prefix}Color`));
    if (call(cell, `is${prefix}RGB`)) {
      params.push(`${extended};2;${(value >> 16) & 255};${(value >> 8) & 255};${value & 255}`);
    } else if (call(cell, `is${prefix}Palette`)) {
      if (value >= 16) params.push(`${extended};5;${value}`);
      else params.push(String(value & 8 ? bright + (value & 7) : base + (value & 7)));
    }
  };
  colour("Fg", 30, 90, 38);
  colour("Bg", 40, 100, 48);
  const flags: [string, string][] = [
    ["isBold", "1"],
    ["isDim", "2"],
    ["isItalic", "3"],
    ["isUnderline", "4"],
    ["isBlink", "5"],
    ["isInverse", "7"],
    ["isInvisible", "8"],
    ["isStrikethrough", "9"],
    ["isOverline", "53"],
  ];
  for (const [method, code] of flags) if (call(cell, method)) params.push(code);
  const chars = call(cell, "getChars");
  return {
    chars: typeof chars === "string" ? chars : "",
    sgr: `\u001b[${params.join(";")}m`,
    width: num(call(cell, "getWidth")),
  };
}
