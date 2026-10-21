// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Limits how many terminals hold a WebGL context at once.
 *
 * Browsers cap live WebGL contexts (about 16 in Chromium and WebKit) and drop
 * the oldest one past the cap, which may belong to the pane in front of the
 * user. The budget hands out fewer slots than that and evicts the least
 * recently attached holder instead, so the evicted terminal is one the user
 * looked at longest ago.
 *
 * A `Map` iterates in insertion order, so deleting and re-inserting a holder
 * moves it to the most recent end: `acquire` and `release` are O(1), and the
 * budget holds O(capacity) entries.
 */
export class WebglBudget {
  readonly #capacity: number;
  readonly #holders = new Map<object, () => void>();

  constructor(capacity: number) {
    if (!Number.isInteger(capacity) || capacity < 1) {
      throw new RangeError(`capacity must be a positive integer, got ${capacity}`);
    }
    this.#capacity = capacity;
  }

  /**
   * Marks `holder` as most recently used, evicting the least recently used
   * holder when the budget is full. `evict` is called at most once, when this
   * holder loses its slot.
   */
  acquire(holder: object, evict: () => void): void {
    if (this.#holders.delete(holder)) {
      this.#holders.set(holder, evict);
      return;
    }
    if (this.#holders.size >= this.#capacity) {
      const oldest = this.#holders.entries().next().value;
      if (oldest) {
        const [oldestHolder, evictOldest] = oldest;
        this.#holders.delete(oldestHolder);
        evictOldest();
      }
    }
    this.#holders.set(holder, evict);
  }

  release(holder: object): void {
    this.#holders.delete(holder);
  }

  get size(): number {
    return this.#holders.size;
  }
}

/** Shared by every terminal in the window. 12 leaves headroom under the ~16 cap. */
export const webglBudget = new WebglBudget(12);
