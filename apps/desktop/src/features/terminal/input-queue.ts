// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

export type InputChunk =
  | { kind: "text"; data: string }
  | { kind: "bytes"; data: Uint8Array }
  /**
   * The saved host's password, which the backend types: in turn with the
   * keys around it, never merged. `done` hears how it went.
   */
  | { kind: "secret"; seen: number; done: (error?: unknown) => void };

export interface InputSink {
  writeText(data: string): Promise<void>;
  writeBytes(data: Uint8Array): Promise<void>;
  fillPassword(seen: number): Promise<void>;
}

/**
 * Sends terminal input in order, with at most one IPC call in flight.
 *
 * Each `session_write` runs as its own task on the backend, so two calls in
 * flight can reach the shell in either order. This queue sends one call at a
 * time and merges everything typed meanwhile into the next call: a fast typist
 * or a paste costs one round trip per burst, not one per key.
 *
 * Adjacent chunks of the same kind merge; text and bytes keep their relative
 * order. The queue is an array with a head index, compacted once the consumed
 * prefix is at least half the array, so `push` and taking a chunk are O(1)
 * amortized. Merging copies each queued byte once: a burst of n bytes in m
 * chunks costs O(n + m) time and O(n) space until it is sent.
 */
export class InputQueue {
  readonly #sink: InputSink;
  readonly #onError: (error: unknown) => void;
  #pending: InputChunk[] = [];
  #head = 0;
  #inFlight = false;
  #closed = false;

  constructor(sink: InputSink, onError: (error: unknown) => void) {
    this.#sink = sink;
    this.#onError = onError;
  }

  push(chunk: InputChunk): void {
    if (this.#closed) {
      if (chunk.kind === "secret") chunk.done(new Error("the pane is closed"));
      return;
    }
    if (chunk.kind !== "secret" && chunk.data.length === 0) {
      return;
    }
    this.#pending.push(chunk);
    if (!this.#inFlight) {
      void this.#drain();
    }
  }

  /** Drops queued input; later pushes still go out. */
  clear(): void {
    this.#drop();
  }

  /** Drops queued input; later pushes are ignored. */
  close(): void {
    this.#closed = true;
    this.#drop();
  }

  #drop(): void {
    for (const chunk of this.#pending.slice(this.#head)) {
      if (chunk.kind === "secret") chunk.done(new Error("dropped"));
    }
    this.#pending = [];
    this.#head = 0;
  }

  get idle(): boolean {
    return !this.#inFlight && this.#head === this.#pending.length;
  }

  async #drain(): Promise<void> {
    this.#inFlight = true;
    try {
      for (let next = this.#takeMerged(); next; next = this.#takeMerged()) {
        if (next.kind === "secret") {
          try {
            // oxlint-disable-next-line no-await-in-loop -- sequential by design: order matters
            await this.#sink.fillPassword(next.seen);
            next.done();
          } catch (error) {
            next.done(error);
          }
          continue;
        }
        try {
          // oxlint-disable-next-line no-await-in-loop -- sequential by design: order matters
          await (next.kind === "text"
            ? this.#sink.writeText(next.data)
            : this.#sink.writeBytes(next.data));
        } catch (error) {
          this.#onError(error);
        }
      }
    } finally {
      this.#inFlight = false;
    }
  }

  /** Removes the leading run of same-kind chunks and returns it as one chunk. */
  #takeMerged(): InputChunk | undefined {
    const first = this.#pending[this.#head];
    if (!first) {
      return undefined;
    }
    if (first.kind === "secret") {
      this.#head += 1;
      return first;
    }
    let end = this.#head + 1;
    while (end < this.#pending.length && this.#pending[end]?.kind === first.kind) {
      end += 1;
    }
    const taken = this.#pending.slice(this.#head, end);
    this.#head = end;
    if (this.#head * 2 >= this.#pending.length) {
      // Each element moves at most once per halving: O(1) amortized.
      this.#pending = this.#pending.slice(this.#head);
      this.#head = 0;
    }
    if (first.kind === "text") {
      let text = "";
      for (const chunk of taken) {
        if (chunk.kind === "text") text += chunk.data;
      }
      return { kind: "text", data: text };
    }
    let total = 0;
    for (const chunk of taken) {
      if (chunk.kind === "bytes") total += chunk.data.length;
    }
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (const chunk of taken) {
      if (chunk.kind === "bytes") {
        bytes.set(chunk.data, offset);
        offset += chunk.data.length;
      }
    }
    return { kind: "bytes", data: bytes };
  }
}

/**
 * xterm.js reports legacy mouse input through `onBinary` as a string whose
 * code units are bytes (0-255). Encoding it as UTF-8 would corrupt values
 * above 127, so map code units to bytes directly. O(n).
 */
export function binaryStringToBytes(data: string): Uint8Array {
  const bytes = new Uint8Array(data.length);
  for (let i = 0; i < data.length; i += 1) {
    bytes[i] = data.charCodeAt(i) & 0xff;
  }
  return bytes;
}
