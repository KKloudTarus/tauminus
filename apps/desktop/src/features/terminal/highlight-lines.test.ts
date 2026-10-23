// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";

import { MAX_SCAN_BYTES, MAX_TEXT_BYTES, segments, visibleLines } from "./highlight-lines.ts";

/** Code points: what places count. */
function points(text: string | undefined): number {
  return text?.match(/./gsu)?.length ?? 0;
}

async function terminal(cols: number, rows: number, text: string): Promise<Terminal> {
  const term = new Terminal({ cols, rows, allowProposedApi: true, scrollback: 100 });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  await new Promise<void>((resolve) => term.write(text, resolve));
  return term;
}

describe("visibleLines", () => {
  it("joins wrapped rows into one line and leaves out trailing blanks", async () => {
    const term = await terminal(10, 4, "0123456789abcdef\r\nshort");
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    // Four screen rows: two wrapped into one line, then two more lines.
    expect(scan.texts).toEqual(["0123456789abcdef", "short", ""]);
    expect(scan.rows[0]).toEqual([0, 1]);
    // A match across the wrap covers the end of one row and the start of the next.
    expect(segments(scan, { line: 0, start: 8, end: 12 }, false, term.cols)).toEqual([
      { row: 0, x: 8, width: 2 },
      { row: 1, x: 0, width: 2 },
    ]);
  });

  it("counts a wide character once and gives it both its cells", async () => {
    const term = await terminal(20, 2, "a漢字b");
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    expect(scan.texts[0]).toBe("a漢字b");
    // Code points 1 to 3 are the two wide characters: four cells.
    expect(segments(scan, { line: 0, start: 1, end: 3 }, false, term.cols)).toEqual([
      { row: 0, x: 1, width: 4 },
    ]);
    expect(segments(scan, { line: 0, start: 3, end: 4 }, false, term.cols)).toEqual([
      { row: 0, x: 5, width: 1 },
    ]);
  });

  it("keeps an emoji outside the basic plane as one code point", async () => {
    const term = await terminal(20, 2, "x😀y ERROR");
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    expect(scan.texts[0]).toBe("x😀y ERROR");
    // "ERROR" starts at code point 4: the emoji is one code point, two cells.
    expect(segments(scan, { line: 0, start: 4, end: 9 }, false, term.cols)).toEqual([
      { row: 0, x: 5, width: 5 },
    ]);
  });

  it("reads the start of a line wrapped from above the screen", async () => {
    // 30 characters at 10 columns, then enough lines to scroll the start away.
    const term = await terminal(10, 2, `${"x".repeat(25)}END`);
    term.scrollToLine(2);
    const buffer = term.buffer.active;
    expect(buffer.viewportY).toBe(1);
    const scan = visibleLines(buffer, term.cols, term.rows);
    expect(scan.texts[0]).toBe(`${"x".repeat(25)}END`);
    expect(scan.rows[0]).toEqual([0, 2]);
  });

  it("colours every row of a wrapped line for a whole-line rule", async () => {
    const term = await terminal(10, 3, "y".repeat(15));
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    expect(segments(scan, { line: 0, start: 0, end: 15 }, true, term.cols)).toEqual([
      { row: 0, x: 0, width: 10 },
      { row: 1, x: 0, width: 10 },
    ]);
    expect(segments(scan, { line: 9, start: 0, end: 1 }, true, term.cols)).toEqual([]);
  });

  it("ignores offsets past the end of the line", async () => {
    const term = await terminal(10, 2, "abc");
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    expect(segments(scan, { line: 0, start: 1, end: 99 }, false, term.cols)).toEqual([
      { row: 0, x: 1, width: 2 },
    ]);
  });

  it("cuts a long line at the backend's limit, keeping places in step", async () => {
    // 24,000 box-drawing characters: 3 bytes and one cell each.
    const term = await terminal(120, 60, "─".repeat(24_000));
    term.scrollToLine(0);
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    const bytes = new TextEncoder().encode(scan.texts[0]).length;
    expect(bytes).toBeLessThanOrEqual(MAX_TEXT_BYTES);
    expect(bytes).toBeGreaterThan(MAX_TEXT_BYTES - 3);
    expect(scan.places[0]).toHaveLength(points(scan.texts[0]));
  });

  it("reads a few code points of a cell however many marks are stacked on it", async () => {
    const term = await terminal(20, 2, `a${"\u0301".repeat(5000)}b`);
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    expect(points(scan.texts[0])).toBeLessThanOrEqual(9);
    expect(scan.texts[0]?.endsWith("b")).toBe(true);
  });

  it("sends at most the scan budget over all its lines", async () => {
    // 120 rows of 200 cells, each a letter and seven marks: 15 bytes a
    // cell, 360 KB a screen.
    const row = `${"x\u0301\u0301\u0301\u0301\u0301\u0301\u0301".repeat(200)}\r\n`;
    const term = await terminal(200, 120, row.repeat(119));
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    const total = scan.texts.reduce((n, t) => n + new TextEncoder().encode(t).length, 0);
    expect(total).toBeLessThanOrEqual(MAX_SCAN_BYTES);
    expect(total).toBeGreaterThan(MAX_SCAN_BYTES - 3000);
  });

  it("joins a wide character pushed to the next row without a gap", async () => {
    // At 5 columns, 漢 does not fit after "abcd" and wraps, leaving a cell.
    const term = await terminal(5, 3, "abcd漢字");
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    expect(scan.texts[0]).toBe("abcd漢字");
    expect(segments(scan, { line: 0, start: 3, end: 5 }, false, term.cols)).toEqual([
      { row: 0, x: 3, width: 1 },
      { row: 1, x: 0, width: 2 },
    ]);
  });

  it("colours only the rows on screen of a line taller than it", async () => {
    const term = await terminal(10, 3, "z".repeat(200));
    term.scrollToLine(10);
    const scan = visibleLines(term.buffer.active, term.cols, term.rows);
    const rows = segments(scan, { line: 0, start: 0, end: 200 }, true, term.cols).map((s) => s.row);
    expect(rows).toEqual([10, 11, 12]);
  });
});
