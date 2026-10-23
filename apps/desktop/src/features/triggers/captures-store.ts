// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** One line a capture rule kept. */
export interface Capture {
  /** Unique in this window, for the list's keys. */
  readonly id: number;
  readonly at: number;
  /** The rule that kept it. */
  readonly rule: string;
  /** The pane it came from, as its tab reads. */
  readonly pane: string;
  /** The line, secrets of a known shape hidden by the backend. */
  readonly text: string;
}

/** Lines kept at most; the oldest go first. */
export const MAX_CAPTURES = 1000;

/**
 * The lines capture rules kept in this window, newest first. In memory
 * only: what a terminal prints is not written to disk outside the vault.
 */
export class CapturesStore {
  readonly #listeners = new Set<() => void>();
  #captures: readonly Capture[] = [];

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): readonly Capture[] => this.#captures;

  #next = 1;

  add(capture: Omit<Capture, "id">): void {
    this.addMany([capture]);
  }

  /** Several at once, newest last: one copy and one change. O(n + k). */
  addMany(captures: readonly Omit<Capture, "id">[]): void {
    const added = captures.map((capture) => ({ ...capture, id: this.#next++ })).toReversed();
    this.#captures = [...added, ...this.#captures].slice(0, MAX_CAPTURES);
    this.#emit();
  }

  clear(): void {
    this.#captures = [];
    this.#emit();
  }

  #emit(): void {
    for (const listener of this.#listeners) listener();
  }
}

/** The least time between two sends of one rule in one pane, in ms. */
export const SEND_EVERY_MS = 2000;
/** Sends of one rule in one pane in a minute, at most. */
export const SENDS_PER_MINUTE = 5;

/**
 * Keeps send rules from typing in a loop: a rule whose text its own output
 * matches again would type forever. Past the limits the rule is off in
 * that pane until the rules change. O(1) per send.
 */
export class SendLimiter {
  readonly #sent = new Map<string, number[]>();
  readonly #off = new Set<string>();
  readonly #every: number;
  readonly #perMinute: number;
  readonly #turnsOff: boolean;

  /** At most one every `every` ms, `perMinute` a minute; past that, off if `turnsOff`. */
  constructor(every = SEND_EVERY_MS, perMinute = SENDS_PER_MINUTE, turnsOff = true) {
    this.#every = every;
    this.#perMinute = perMinute;
    this.#turnsOff = turnsOff;
  }

  /** Whether rule `key` may send at `now`, counting it if so; `"off"` the first time it may not. */
  allow(key: string, now: number): boolean | "off" {
    if (this.#off.has(key)) return false;
    const times = (this.#sent.get(key) ?? []).filter((at) => now - at < 60_000);
    const last = times.at(-1);
    if ((last !== undefined && now - last < this.#every) || times.length >= this.#perMinute) {
      if (!this.#turnsOff) return false;
      this.#off.add(key);
      return "off";
    }
    times.push(now);
    this.#sent.set(key, times);
    return true;
  }

  /** The rules changed: every rule may send again. */
  reset(): void {
    this.#sent.clear();
    this.#off.clear();
  }
}
