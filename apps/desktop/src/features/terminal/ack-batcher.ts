// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Coalesces flow-control acknowledgements so the view sends one IPC call per
 * `threshold` bytes instead of one per batch.
 *
 * Invariant: `threshold` must stay below the backend's low watermark L
 * (96 KiB, `Watermarks::default()` in tauminus-domain). After the view has
 * parsed everything, at most `threshold - 1` bytes stay unacknowledged, and
 * `threshold - 1 < L` lets a paused session resume.
 *
 * `add` is O(1).
 */
export class AckBatcher {
  readonly #threshold: number;
  #pending = 0;

  constructor(threshold: number) {
    if (!Number.isInteger(threshold) || threshold < 1) {
      throw new RangeError(`threshold must be a positive integer, got ${threshold}`);
    }
    this.#threshold = threshold;
  }

  /** Records parsed bytes. Returns the count to acknowledge now, or 0. */
  add(bytes: number): number {
    this.#pending += bytes;
    if (this.#pending < this.#threshold) {
      return 0;
    }
    const ready = this.#pending;
    this.#pending = 0;
    return ready;
  }

  get pending(): number {
    return this.#pending;
  }
}

/**
 * 64 KiB: equal to one full output batch, so a flood costs one ack per batch,
 * and below the 96 KiB low watermark as the invariant requires.
 */
export const ACK_THRESHOLD_BYTES = 64 * 1024;

/** The backend's low watermark; kept in sync with `Watermarks::default()`. */
export const BACKEND_LOW_WATERMARK_BYTES = 96 * 1024;
