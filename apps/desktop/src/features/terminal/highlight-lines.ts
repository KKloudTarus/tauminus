// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The text a pane shows, as the lines highlight rules match, and where each
 * character of it sits on the grid. A line is a logical line: rows joined
 * where the terminal wrapped them, so a match across a wrap is found.
 * Matching runs in the backend; these turn its answer back into cells.
 */

/** The parts of an xterm.js buffer read here. */
export interface GridBuffer {
  readonly viewportY: number;
  readonly length: number;
  getLine(y: number): GridLine | undefined;
}

export interface GridLine {
  readonly isWrapped: boolean;
  getCell(x: number): GridCell | undefined;
}

export interface GridCell {
  getChars(): string;
  getWidth(): number;
}

/** Where one code point of a line sits: buffer row, column, cell width. */
export interface Place {
  readonly row: number;
  readonly col: number;
  readonly width: number;
}

export interface ScanLines {
  /** One per logical line, for the backend to match. */
  readonly texts: string[];
  /** Per line, the place of each code point of its text. */
  readonly places: Place[][];
  /** Per line, its first and last buffer row. */
  readonly rows: [number, number][];
  /** The rows on screen, first and last; decorations go only there. */
  readonly screen: [number, number];
}

/** Rows read past the screen to finish a line wrapped off its bottom. */
const MORE_ROWS = 64;
/** The most of one line sent to match, in UTF-8 bytes: the backend's limit. */
export const MAX_TEXT_BYTES = 16 * 1024;
/** The most text one scan sends, in UTF-8 bytes: the backend's budget. */
export const MAX_SCAN_BYTES = 256 * 1024;
/** Code points read from one cell. */
const MAX_CELL_POINTS = 8;

/** UTF-8 bytes of one code point. */
function utf8Length(point: string): number {
  const code = point.codePointAt(0) ?? 0;
  return code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
}

/**
 * The logical lines that show in a viewport of `rows` rows of `cols`
 * cells, including the start of one wrapped from above the screen and the
 * rest of one wrapped below it. Empty cells read as spaces, but those at
 * the end of a row are left out: a wide character that did not fit moves
 * to the next row and leaves one, and the line the user reads has no gap
 * there. A line is cut at [`MAX_TEXT_BYTES`], a cell at its first few code
 * points, and the whole at [`MAX_SCAN_BYTES`]. O(rows × cols), plus up to
 * [`MORE_ROWS`] rows at each edge for wrapped lines.
 */
export function visibleLines(buffer: GridBuffer, cols: number, rows: number): ScanLines {
  const texts: string[] = [];
  const places: Place[][] = [];
  const spans: [number, number][] = [];
  let first = buffer.viewportY;
  for (let back = 0; back < MORE_ROWS && first > 0 && buffer.getLine(first)?.isWrapped; back += 1) {
    first -= 1;
  }
  const bottom = Math.min(buffer.viewportY + rows, buffer.length) - 1;
  let row = first;
  let total = 0;
  while (row <= bottom && total < MAX_SCAN_BYTES) {
    const start = row;
    let text = "";
    const where: Place[] = [];
    let bytes = 0;
    let full = false;
    const limit = Math.min(MAX_TEXT_BYTES, MAX_SCAN_BYTES - total);
    do {
      const line = buffer.getLine(row);
      const across = line === undefined || full ? 0 : cols;
      // This row's code points up to its last non-empty cell, and theirs.
      const points: string[] = [];
      const rowWhere: Place[] = [];
      let kept = 0;
      for (let col = 0; col < across; col += 1) {
        const cell = line?.getCell(col);
        const width = cell?.getWidth() ?? 1;
        // The second half of a wide character.
        if (width === 0) continue;
        const chars = cell?.getChars() ?? "";
        // A cell holds a character and the marks combined with it; a host
        // can stack thousands. The first few are what anyone matches.
        let inCell = 0;
        for (const point of chars === "" ? " " : chars) {
          if (inCell === MAX_CELL_POINTS) break;
          inCell += 1;
          points.push(point);
          rowWhere.push({ row, col, width });
        }
        if (chars !== "") kept = points.length;
      }
      for (let at = 0; at < kept; at += 1) {
        const point = points[at] ?? "";
        const size = utf8Length(point);
        if (bytes + size > limit) {
          full = true;
          break;
        }
        bytes += size;
        text += point;
        const place = rowWhere[at];
        if (place) where.push(place);
      }
      row += 1;
    } while (row < buffer.length && row <= bottom + MORE_ROWS && buffer.getLine(row)?.isWrapped);
    total += bytes;
    texts.push(text);
    places.push(where);
    spans.push([start, row - 1]);
  }
  return { texts, places, rows: spans, screen: [buffer.viewportY, bottom] };
}

/** A run of cells on one row. */
export interface Segment {
  readonly row: number;
  readonly x: number;
  readonly width: number;
}

/**
 * The cells on screen code points `start` to `end` of line `line` cover,
 * one segment per row; a whole-line highlight covers every row of the line
 * on screen in full. O(end − start).
 */
export function segments(
  scan: ScanLines,
  hit: { line: number; start: number; end: number },
  whole: boolean,
  cols: number,
): Segment[] {
  const [top, bottom] = scan.screen;
  if (whole) {
    const span = scan.rows[hit.line];
    if (!span) return [];
    const out: Segment[] = [];
    for (let row = Math.max(span[0], top); row <= Math.min(span[1], bottom); row += 1) {
      out.push({ row, x: 0, width: cols });
    }
    return out;
  }
  const where = scan.places[hit.line] ?? [];
  const out: { row: number; x: number; width: number }[] = [];
  for (let i = hit.start; i < Math.min(hit.end, where.length); i += 1) {
    const place = where[i];
    // Off screen: nothing to draw.
    if (!place || place.row < top || place.row > bottom) continue;
    const last = out.at(-1);
    const end = place.col + place.width;
    if (last && last.row === place.row && place.col <= last.x + last.width) {
      last.width = Math.max(last.width, end - last.x);
    } else {
      out.push({ row: place.row, x: place.col, width: place.width });
    }
  }
  return out;
}
