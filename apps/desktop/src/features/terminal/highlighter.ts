// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Colours what highlight rules match on a pane's screen. The rules and the
 * matching stay in the backend; this sends the lines that show and draws
 * xterm.js decorations where the answer says.
 *
 * Only what shows is matched and decorated: every xterm.js marker costs
 * each scrolled line a call, so decorations kept in scrollback would slow
 * output down. A scan runs at most every 100 ms while output or scrolling
 * goes on, one at a time, and draws at most [`MAX_DECORATIONS`].
 */

import type { IDecorationOptions, IDisposable, IMarker, ITheme } from "@xterm/xterm";

/** A decoration, as far as a highlighter keeps it. */
export interface HighlightDecoration {
  dispose(): void;
}

/** The parts of a terminal a highlighter uses; an xterm.js `Terminal` has them. */
export interface HighlightTerminal {
  readonly buffer: {
    readonly active: GridBuffer & { readonly baseY: number; readonly cursorY: number };
    onBufferChange(listener: () => void): IDisposable;
  };
  readonly cols: number;
  readonly rows: number;
  readonly options: { readonly theme?: ITheme | undefined };
  readonly element: HTMLElement | undefined;
  onWriteParsed(listener: () => void): IDisposable;
  onScroll(listener: () => void): IDisposable;
  onResize(listener: () => void): IDisposable;
  registerMarker(cursorYOffset: number): IMarker | undefined;
  registerDecoration(options: IDecorationOptions): HighlightDecoration | undefined;
}

import type { HitView, PreparedView, StyleSpec } from "@/ipc/index.ts";

import { type GridBuffer, type ScanLines, segments, visibleLines } from "./highlight-lines.ts";

/** What a pane's rules come from: its saved host's, or the defaults. */
export interface HighlightSource {
  prepare(): Promise<PreparedView>;
  /** `null` when the rules changed since `prepare`. */
  scan(handle: number, lines: string[]): Promise<HitView[] | null>;
}

/**
 * Decorations on screen at most. Each holds a marker, which every scrolled
 * line calls: 256 halved a flood's speed, 51 cost 16%.
 */
export const MAX_DECORATIONS = 128;
/** The least time between two scans, in ms. */
const EVERY_MS = 100;
/** The time between scans while more than a screen scrolls between them. */
const BUSY_MS = 400;

/** One decoration and the marker it owns. */
interface Drawn {
  readonly decoration: HighlightDecoration;
  readonly marker: IMarker;
  readonly x: number;
  readonly width: number;
  readonly look: string;
}

/** xterm.js's default palette, for a theme that sets none. */
const PALETTE = [
  "#2e3436",
  "#cc0000",
  "#4e9a06",
  "#c4a000",
  "#3465a4",
  "#75507b",
  "#06989a",
  "#d3d7cf",
  "#555753",
  "#ef2929",
  "#8ae234",
  "#fce94f",
  "#729fcf",
  "#ad7fa8",
  "#34e2e2",
  "#eeeeec",
] as const;

const THEME_NAMES = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
  "brightWhite",
] as const satisfies readonly (keyof ITheme)[];

const HEX = /^#[0-9a-f]{6}$/iu;

/**
 * `"0"`–`"15"` as the theme's palette colour, `"#rrggbb"` as itself, in the
 * `#rrggbb` form decorations take; `undefined` for none or another form.
 */
export function colourOf(
  text: string | null | undefined,
  theme: ITheme | undefined,
): string | undefined {
  if (!text) return undefined;
  if (HEX.test(text)) return text;
  if (!/^\d{1,2}$/u.test(text)) return undefined;
  const index = Number(text);
  const name = THEME_NAMES[index];
  if (name === undefined) return undefined;
  const themed = theme?.[name];
  if (typeof themed === "string" && HEX.test(themed)) return themed;
  return PALETTE[index];
}

/** Where and how a decoration draws: two with the same key look the same. */
function decorationKey(row: number, x: number, width: number, look: string): string {
  return `${row}:${x}:${width}:${look}`;
}

export class Highlighter {
  readonly #term: HighlightTerminal;
  readonly #source: HighlightSource;
  #prepared: PreparedView | undefined;
  #preparing: Promise<void> | undefined;
  /** Bumped by `refresh`: an answer to a prepare from before is dropped. */
  #generation = 0;
  /** The last prepare failed: none again until `refresh`. */
  #failed = false;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #scanning = false;
  /** Something changed while a scan was out: scan again after it. */
  #again = false;
  #drawn: Drawn[] = [];
  /** Where the buffer stood at the last scan, to tell a flood from a trickle. */
  #lastBase = 0;
  #busy = false;
  /** A scan was due while the pane was hidden. */
  #waiting = false;
  readonly #listeners: IDisposable[] = [];
  #disposed = false;

  constructor(term: HighlightTerminal, source: HighlightSource) {
    this.#term = term;
    this.#source = source;
    const schedule = () => this.#schedule();
    this.#listeners.push(
      term.onWriteParsed(schedule),
      term.onScroll(schedule),
      term.onResize(schedule),
      // The other buffer's rows carry the same numbers: what was drawn for
      // one would colour the other until the next scan.
      term.buffer.onBufferChange(() => {
        this.#clear();
        schedule();
      }),
    );
    this.refresh();
  }

  /** The rules changed: compile them again, then scan. */
  refresh(): void {
    this.#generation += 1;
    this.#prepared = undefined;
    this.#preparing = undefined;
    this.#failed = false;
    this.#schedule();
  }

  dispose(): void {
    this.#disposed = true;
    clearTimeout(this.#timer);
    for (const listener of this.#listeners) listener.dispose();
    this.#clear();
  }

  #schedule(): void {
    if (this.#disposed) return;
    if (this.#scanning) {
      this.#again = true;
      return;
    }
    if (this.#timer !== undefined) return;
    this.#timer = setTimeout(
      () => {
        this.#timer = undefined;
        void this.#run();
      },
      this.#busy ? BUSY_MS : EVERY_MS,
    );
  }

  /** The pane shows again: scan if one was due while it was hidden. */
  wake(): void {
    if (!this.#waiting) return;
    this.#waiting = false;
    this.#schedule();
  }

  async #prepare(): Promise<PreparedView | undefined> {
    if (this.#prepared) return this.#prepared;
    // Output does not change the answer: the rules or the host must.
    if (this.#failed) return undefined;
    const generation = this.#generation;
    this.#preparing ??= this.#source.prepare().then(
      (prepared) => {
        if (generation === this.#generation) this.#prepared = prepared;
      },
      (error: unknown) => {
        if (generation === this.#generation) this.#failed = true;
        console.warn("could not prepare highlight rules", error);
      },
    );
    await this.#preparing;
    return generation === this.#generation ? this.#prepared : undefined;
  }

  async #run(): Promise<void> {
    this.#scanning = true;
    this.#again = false;
    // Only for the scan: disposed when it ends.
    let anchor: IMarker | undefined;
    try {
      const prepared = await this.#prepare();
      if (this.#disposed) return;
      if (!prepared || prepared.styles.length === 0) {
        this.#clear();
        return;
      }
      // A pane taken off the page shows nothing to colour.
      const element = this.#term.element;
      if (element && !element.isConnected) {
        this.#waiting = true;
        return;
      }
      const buffer = this.#term.buffer.active;
      this.#busy = Math.abs(buffer.baseY - this.#lastBase) > this.#term.rows;
      this.#lastBase = buffer.baseY;
      const scan = visibleLines(buffer, this.#term.cols, this.#term.rows);
      // Follows the first row as lines are trimmed from the top of the
      // scrollback while the answer is out.
      anchor = this.#mark(scan.rows[0]?.[0] ?? buffer.viewportY);
      const hits = await this.#source.scan(prepared.handle, scan.texts);
      if (this.#disposed) return;
      if (hits === null) {
        this.refresh();
        return;
      }
      this.#draw(scan, hits, prepared.styles, anchor);
    } catch (error) {
      // What is drawn may no longer match the screen.
      this.#clear();
      console.warn("could not highlight", error);
    } finally {
      anchor?.dispose();
      this.#scanning = false;
      if (this.#again) this.#schedule();
    }
  }

  /** A marker on buffer row `row`. */
  #mark(row: number): IMarker | undefined {
    const buffer = this.#term.buffer.active;
    return this.#term.registerMarker(row - (buffer.baseY + buffer.cursorY));
  }

  /**
   * Draws `hits`, keeping what is drawn already where it stays the same:
   * each change of a decoration redraws the whole screen, and most hits
   * are where they were a scan ago. O(H + D).
   */
  #draw(scan: ScanLines, hits: HitView[], styles: StyleSpec[], anchor: IMarker | undefined): void {
    const firstRow = scan.rows[0]?.[0];
    if (!anchor || anchor.isDisposed || firstRow === undefined) {
      this.#clear();
      return;
    }
    // Rows trimmed off the top since the scan moved every row up.
    const shift = anchor.line - firstRow;
    const theme = this.#term.options.theme;
    const cols = this.#term.cols;
    // What to draw, by where and how, in order.
    const wanted = new Map<
      string,
      {
        row: number;
        x: number;
        width: number;
        look: string;
        options: Omit<IDecorationOptions, "marker">;
      }
    >();
    for (const hit of hits) {
      if (wanted.size >= MAX_DECORATIONS) break;
      const style = styles[hit.style];
      if (!style) continue;
      const background = colourOf(style.bg, theme);
      const foreground = colourOf(style.fg, theme);
      if (!background && !foreground) continue;
      const layer = style.line ? "bottom" : "top";
      const look = `${background ?? ""}/${foreground ?? ""}/${layer}`;
      for (const segment of segments(scan, hit, style.line === true, cols)) {
        if (wanted.size >= MAX_DECORATIONS) break;
        const row = segment.row + shift;
        wanted.set(decorationKey(row, segment.x, segment.width, look), {
          row,
          x: segment.x,
          width: segment.width,
          look,
          options: {
            x: segment.x,
            width: segment.width,
            ...(background ? { backgroundColor: background } : {}),
            ...(foreground ? { foregroundColor: foreground } : {}),
            layer,
          },
        });
      }
    }
    const kept: Drawn[] = [];
    for (const drawn of this.#drawn) {
      const at = drawn.marker.isDisposed
        ? undefined
        : decorationKey(drawn.marker.line, drawn.x, drawn.width, drawn.look);
      if (at !== undefined && wanted.delete(at)) {
        kept.push(drawn);
      } else {
        drawn.decoration.dispose();
        drawn.marker.dispose();
      }
    }
    for (const { row, x, width, look, options } of wanted.values()) {
      const marker = this.#mark(row);
      if (!marker) continue;
      const decoration = this.#term.registerDecoration({ marker, ...options });
      if (decoration) {
        kept.push({ decoration, marker, x, width, look });
      } else {
        marker.dispose();
      }
    }
    this.#drawn = kept;
  }

  #clear(): void {
    for (const drawn of this.#drawn) {
      drawn.decoration.dispose();
      drawn.marker.dispose();
    }
    this.#drawn = [];
  }

  /** Decorations and markers held now; for tests. */
  get held(): { decorations: number; markers: number } {
    return { decorations: this.#drawn.length, markers: this.#drawn.length };
  }
}
