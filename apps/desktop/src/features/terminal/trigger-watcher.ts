// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Runs trigger rules (notify, capture, send) on a pane's output. The rules
 * and the matching stay in the backend; this sends lines and hands what
 * fired on, a scan at a time.
 *
 * Notify and capture read each finished line once, as it scrolls in: a
 * line is finished once the cursor has left it, so a progress bar redrawn
 * with `\r` fires once it is done. A scan runs at most every 100 ms and
 * reads at most [`MAX_ROWS`] rows: a flood faster than that skips lines,
 * since triggers watch, they do not guard.
 *
 * Send reads the line the cursor is on, once output has stopped for
 * [`IDLE_MS`]: a question such as `Continue? [y/N] ` waits there for its
 * answer. Each such line is read once. Waiting for quiet also lets a
 * password prompt that follows a line arrive before anything is typed.
 *
 * Full-screen programs (the alternate screen) are skipped: they redraw
 * constantly, and their lines are not output that ends.
 */

import type { IDisposable, IMarker } from "@xterm/xterm";

import type { FiredView, PreparedView, TriggerView } from "@/ipc/index.ts";

/** What a pane's trigger rules come from. */
export interface TriggerSource {
  prepare(): Promise<PreparedView>;
  /** `null` when the rules changed since `prepare`. */
  fire(handle: number, lines: string[]): Promise<FiredView[] | null>;
}

export interface WatchLine {
  readonly isWrapped: boolean;
  translateToString(trimRight?: boolean): string;
}

/** The parts of a terminal a watcher uses; an xterm.js `Terminal` has them. */
export interface WatchTerminal {
  readonly buffer: {
    readonly active: {
      readonly type: "normal" | "alternate";
      readonly baseY: number;
      readonly cursorY: number;
      readonly length: number;
      getLine(y: number): WatchLine | undefined;
    };
    onBufferChange(listener: () => void): IDisposable;
  };
  onWriteParsed(listener: () => void): IDisposable;
  registerMarker(cursorYOffset: number): IMarker | undefined;
}

/** Rows one scan reads at most. */
export const MAX_ROWS = 512;
/** How long output must stop before the cursor's line is read for send rules, in ms. */
export const IDLE_MS = 300;
/** Length of one line sent, in UTF-16 units. */
const MAX_LINE = 16 * 1024;
/** Length of all lines one scan sends, in UTF-16 units: under the backend's limit. */
const MAX_TOTAL = 256 * 1024;
const EVERY_MS = 100;

/** What one scan fired, with the rules it fired by. */
export interface FiredBatch {
  readonly prepared: PreparedView;
  readonly fired: readonly FiredView[];
}

export class TriggerWatcher {
  readonly #term: WatchTerminal;
  readonly #source: TriggerSource;
  readonly #fired: (batch: FiredBatch) => void;
  #idle: ReturnType<typeof setTimeout> | undefined;
  /** The waiting line last read for send rules, by where it starts and what it says. */
  #waiting = "";
  #prepared: PreparedView | undefined;
  #preparing: Promise<void> | undefined;
  #generation = 0;
  #failed = false;
  /** The first row not read yet, on the normal screen. */
  #next: IMarker | undefined;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #running = false;
  #again = false;
  readonly #listeners: IDisposable[] = [];
  #disposed = false;

  constructor(term: WatchTerminal, source: TriggerSource, fired: (batch: FiredBatch) => void) {
    this.#term = term;
    this.#source = source;
    this.#fired = fired;
    // What the pane showed before is not news: start where the cursor is.
    this.#next = term.registerMarker(0);
    const schedule = () => {
      this.#schedule();
      clearTimeout(this.#idle);
      this.#idle = setTimeout(() => void this.#runWaiting(), IDLE_MS);
    };
    this.#listeners.push(term.onWriteParsed(schedule), term.buffer.onBufferChange(schedule));
  }

  /** The rules changed: compile them again. */
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
    clearTimeout(this.#idle);
    for (const listener of this.#listeners) listener.dispose();
    this.#next?.dispose();
  }

  #schedule(): void {
    if (this.#disposed) return;
    if (this.#running) {
      this.#again = true;
      return;
    }
    if (this.#timer !== undefined) return;
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      void this.#run();
    }, EVERY_MS);
  }

  async #prepare(): Promise<PreparedView | undefined> {
    if (this.#prepared) return this.#prepared;
    if (this.#failed) return undefined;
    const generation = this.#generation;
    this.#preparing ??= this.#source.prepare().then(
      (prepared) => {
        if (generation === this.#generation) this.#prepared = prepared;
      },
      (error: unknown) => {
        if (generation === this.#generation) this.#failed = true;
        console.warn("could not prepare trigger rules", error);
      },
    );
    await this.#preparing;
    return generation === this.#generation ? this.#prepared : undefined;
  }

  async #run(): Promise<void> {
    this.#running = true;
    this.#again = false;
    try {
      let prepared = await this.#prepare();
      if (this.#disposed || this.#term.buffer.active.type === "alternate") return;
      // No trigger reads finished lines: they go by unread.
      const lines = this.#take(prepared !== undefined && prepared.triggers.some(finishedLines));
      if (!prepared || lines.length === 0) return;
      let fired = await this.#source.fire(prepared.handle, lines);
      if (fired === null) {
        // The rules changed while the lines waited: once more, with the new.
        this.refresh();
        prepared = await this.#prepare();
        if (this.#disposed || !prepared) return;
        fired = (await this.#source.fire(prepared.handle, lines)) ?? [];
      }
      const kept = prepared.triggers;
      this.#hand(
        prepared,
        fired.filter((one) => finishedLines(kept[one.rule])),
      );
    } catch (error) {
      console.warn("could not run triggers", error);
    } finally {
      this.#running = false;
      if (this.#again) this.#schedule();
    }
  }

  #hand(prepared: PreparedView, fired: readonly FiredView[]): void {
    if (fired.length > 0) this.#fired({ prepared, fired });
  }

  /** Reads the line the cursor waits on, for send rules, once per line. */
  async #runWaiting(): Promise<void> {
    if (this.#disposed || this.#term.buffer.active.type === "alternate") return;
    try {
      const prepared = await this.#prepare();
      if (this.#disposed || !prepared?.triggers.some((t) => !finishedLines(t))) return;
      const waiting = this.#waitingLine();
      if (waiting === undefined || waiting.key === this.#waiting) return;
      this.#waiting = waiting.key;
      const fired = (await this.#source.fire(prepared.handle, [waiting.text])) ?? [];
      // Only if the line is still the one waiting: output may have come.
      if (this.#disposed || this.#waitingLine()?.key !== waiting.key) return;
      this.#hand(
        prepared,
        fired.filter((one) => !finishedLines(prepared.triggers[one.rule])),
      );
    } catch (error) {
      console.warn("could not run triggers", error);
    }
  }

  /** The cursor's line up to the cursor, and a key for it. */
  #waitingLine(): { key: string; text: string } | undefined {
    const buffer = this.#term.buffer.active;
    const cursor = buffer.baseY + buffer.cursorY;
    let start = cursor;
    while (start > 0 && buffer.getLine(start)?.isWrapped) start -= 1;
    let text = "";
    for (let row = start; row <= cursor; row += 1) {
      text += buffer.getLine(row)?.translateToString(row === cursor) ?? "";
    }
    const cut = text.slice(0, MAX_LINE);
    if (cut.trim() === "") return undefined;
    return { key: `${start}:${cut}`, text: cut };
  }

  /**
   * The finished lines since the last scan, read if `read`, and the mark
   * moved past them. O(rows read × cols).
   */
  #take(read: boolean): string[] {
    const buffer = this.#term.buffer.active;
    const cursor = buffer.baseY + buffer.cursorY;
    // The line the cursor is in is not finished, from its first row.
    let limit = cursor;
    while (limit > 0 && buffer.getLine(limit)?.isWrapped) limit -= 1;
    const marked = this.#next;
    // A mark gone with its row: the screen was erased (Ctrl+L), or the row
    // trimmed off the top in a flood. Either way, what was above is seen.
    if (!marked || marked.isDisposed) {
      this.#next = this.#term.registerMarker(limit - cursor);
      return [];
    }
    let start = marked.line;
    if (start >= limit) return [];
    if (!read) {
      marked.dispose();
      this.#next = this.#term.registerMarker(limit - cursor);
      return [];
    }
    if (limit - start > MAX_ROWS) {
      start = limit - MAX_ROWS;
      // From the next line's first row.
      while (start < limit && buffer.getLine(start)?.isWrapped) start += 1;
    }
    const lines: string[] = [];
    let total = 0;
    let row = start;
    // Each row fetched once: a fetch makes a view of it.
    let line = buffer.getLine(row);
    while (row < limit && total < MAX_TOTAL) {
      let text = "";
      for (;;) {
        row += 1;
        const next = row < limit ? buffer.getLine(row) : undefined;
        const continues = next?.isWrapped === true;
        // A wrapped row's trailing blanks are the line's own spaces.
        text += line?.translateToString(!continues) ?? "";
        line = next;
        if (!continues) break;
      }
      const cut = text.slice(0, Math.min(MAX_LINE, MAX_TOTAL - total));
      total += cut.length;
      lines.push(cut);
    }
    marked.dispose();
    this.#next = this.#term.registerMarker(limit - cursor);
    return lines;
  }
}

/** Whether a rule reads finished lines (notify, capture) rather than the waiting one (send). */
function finishedLines(trigger: TriggerView | undefined): boolean {
  return trigger !== undefined && trigger.action.kind !== "send";
}
