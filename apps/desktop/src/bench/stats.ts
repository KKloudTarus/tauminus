// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** One display refresh at 60 Hz, in ms. */
export const FRAME_MS = 1000 / 60;

/**
 * Counts animation frames and the frames lost between them. A gap of g ms
 * between two callbacks stands for round(g / 16.7) frames, of which all but
 * one were dropped. O(1) per frame.
 */
export class FrameCounter {
  frames = 0;
  dropped = 0;
  maxGapMs = 0;
  #last: number | undefined;

  tick(at: number): void {
    if (this.#last !== undefined) {
      const gap = at - this.#last;
      this.frames += 1;
      this.dropped += Math.max(0, Math.round(gap / FRAME_MS) - 1);
      this.maxGapMs = Math.max(this.maxGapMs, gap);
    }
    this.#last = at;
  }

  /** Dropped frames as a share of the frames the display showed or should have. */
  get droppedShare(): number {
    const expected = this.frames + this.dropped;
    return expected === 0 ? 0 : this.dropped / expected;
  }
}

export interface Summary {
  n: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
}

/** Nearest-rank percentiles of `samples`. O(n log n) for the sort. */
export function summarize(samples: readonly number[]): Summary {
  const sorted = samples.filter((s) => Number.isFinite(s)).toSorted((a, b) => a - b);
  const rank = (q: number) => sorted[Math.max(0, Math.ceil(q * sorted.length) - 1)] ?? Number.NaN;
  return {
    n: sorted.length,
    p50: rank(0.5),
    p95: rank(0.95),
    p99: rank(0.99),
    max: sorted.at(-1) ?? Number.NaN,
  };
}

/** Lowercase hex of UTF-8 text: survives any shell without quoting. */
export function toHex(text: string): string {
  return Array.from(new TextEncoder().encode(text), (b) => b.toString(16).padStart(2, "0")).join(
    "",
  );
}
