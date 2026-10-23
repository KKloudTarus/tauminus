// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** The page's side of the player worker: requests as promises. */

import type { CastHeader } from "./cast.ts";
import type { Frame, Hit, PlayEvent } from "./player-core.ts";
import type { Reply, Request } from "./player-protocol.ts";

export interface Loaded {
  readonly header: CastHeader;
  readonly count: number;
  readonly duration: number;
  readonly times: Float64Array;
}

/** What the engine needs of a worker, so tests can give it another. */
export interface WorkerLike {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (event: MessageEvent<Reply>) => void): void;
  addEventListener(type: "error", listener: (event: ErrorEvent) => void): void;
  terminate(): void;
}

function playerWorker(): WorkerLike {
  return new Worker(new URL("./player.worker.ts", import.meta.url), { type: "module" });
}

type Pending = { resolve: (reply: Reply) => void; reject: (error: Error) => void };

export class PlayerEngine {
  readonly #worker: WorkerLike;
  readonly #pending = new Map<number, Pending>();
  #next = 1;
  onProgress: (share: number) => void = () => {};
  /** Checkpoints built this time, to keep for the next. */
  onBuilt: (checkpoints: string) => void = () => {};

  constructor(worker: WorkerLike = playerWorker()) {
    this.#worker = worker;
    worker.addEventListener("message", (event) => this.#reply(event.data));
    // A worker that fails to start, or throws outside a request, fails
    // what waits on it instead of leaving it waiting.
    worker.addEventListener("error", (event) => {
      const error = new Error(event.message || "the player stopped");
      for (const pending of this.#pending.values()) pending.reject(error);
      this.#pending.clear();
    });
  }

  #reply(reply: Reply): void {
    if (reply.type === "progress") {
      this.onProgress(reply.share);
      return;
    }
    if (reply.type === "built") {
      this.onBuilt(reply.checkpoints);
      return;
    }
    const pending = this.#pending.get(reply.id);
    if (!pending) return;
    this.#pending.delete(reply.id);
    if (reply.type === "failed") pending.reject(new Error(reply.message));
    else pending.resolve(reply);
  }

  #ask(request: Request, transfer: Transferable[] = []): Promise<Reply> {
    return new Promise((resolve, reject) => {
      this.#pending.set(request.id, { resolve, reject });
      this.#worker.postMessage(request, transfer);
    });
  }

  #id(): number {
    const id = this.#next;
    this.#next += 1;
    return id;
  }

  /** Hands `bytes` to the worker, which keeps them; the caller's copy goes. */
  async load(bytes: Uint8Array, saved: string | undefined, complete: boolean): Promise<Loaded> {
    const buffer =
      bytes.byteOffset === 0 &&
      bytes.byteLength === bytes.buffer.byteLength &&
      bytes.buffer instanceof ArrayBuffer
        ? bytes.buffer
        : bytes.slice().buffer;
    const reply = await this.#ask(
      { type: "load", id: this.#id(), bytes: buffer, saved, complete },
      [buffer],
    );
    if (reply.type !== "loaded") throw new Error("the player did not load");
    return reply;
  }

  async seek(event: number): Promise<Frame> {
    const reply = await this.#ask({ type: "seek", id: this.#id(), event });
    if (reply.type !== "frame") throw new Error("the player did not seek");
    return reply.frame;
  }

  async events(from: number, until: number): Promise<PlayEvent[]> {
    const reply = await this.#ask({ type: "events", id: this.#id(), from, until });
    if (reply.type !== "events") throw new Error("the player sent no events");
    return reply.events;
  }

  async search(query: string): Promise<Hit[]> {
    const reply = await this.#ask({ type: "search", id: this.#id(), query });
    if (reply.type !== "found") throw new Error("the player did not search");
    return reply.hits;
  }

  dispose(): void {
    this.#worker.terminate();
    for (const pending of this.#pending.values()) pending.reject(new Error("closed"));
    this.#pending.clear();
  }
}

/** The first event after `seconds`, in `times`. O(log n). */
export function eventAt(times: Float64Array, seconds: number): number {
  let low = 0;
  let high = times.length;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if ((times[mid] ?? 0) <= seconds) low = mid + 1;
    else high = mid;
  }
  return low;
}
