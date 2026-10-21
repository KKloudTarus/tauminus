// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Whether to drop a REP sequence (CSI n b, repeat the last character n times).
 *
 * xterm.js 6 builds REP's output as one typed array of n cells and then loops
 * n times. Fifteen bytes from a remote host, `A ESC [ 2147483647 b`, ask for
 * 8 GiB: V8 stalls or runs out of memory, and JavaScriptCore throws inside
 * the parser, after which that terminal never parses again. ncurses sends REP
 * only for runs within a line, so a count past one screenful comes from
 * broken or hostile output, and dropping it costs nothing real.
 *
 * O(1).
 */
export function dropsRepeat(params: (number | number[])[], cols: number, rows: number): boolean {
  const first = params[0];
  const count = typeof first === "number" && first > 0 ? first : 1;
  return count > cols * rows;
}
