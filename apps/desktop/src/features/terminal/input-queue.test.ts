// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import { InputQueue, type InputSink, binaryStringToBytes } from "./input-queue.ts";

/** A sink whose calls resolve only when the test says so. */
class ManualSink implements InputSink {
  calls: { kind: "text" | "bytes" | "secret"; data: string | number[] }[] = [];
  #resolvers: (() => void)[] = [];
  failNext = false;

  writeText(data: string): Promise<void> {
    return this.#record({ kind: "text", data });
  }

  writeBytes(data: Uint8Array): Promise<void> {
    return this.#record({ kind: "bytes", data: [...data] });
  }

  fillPassword(seen: number): Promise<void> {
    return this.#record({ kind: "secret", data: String(seen) });
  }

  #record(call: (typeof this.calls)[number]): Promise<void> {
    this.calls.push(call);
    const fail = this.failNext;
    this.failNext = false;
    return new Promise((resolve, reject) => {
      this.#resolvers.push(() => (fail ? reject(new Error("write failed")) : resolve()));
    });
  }

  async complete(): Promise<void> {
    this.#resolvers.shift()?.();
    await Promise.resolve();
    await Promise.resolve();
  }
}

const text = (data: string) => ({ kind: "text", data }) as const;
const bytes = (...values: number[]) => ({ kind: "bytes", data: new Uint8Array(values) }) as const;

describe("InputQueue", () => {
  it("keeps one write in flight and merges what arrives meanwhile", async () => {
    const sink = new ManualSink();
    const queue = new InputQueue(sink, vi.fn<(error: unknown) => void>());
    queue.push(text("l"));
    queue.push(text("s"));
    queue.push(text("\r"));
    expect(sink.calls).toEqual([{ kind: "text", data: "l" }]);

    await sink.complete();
    expect(sink.calls).toEqual([
      { kind: "text", data: "l" },
      { kind: "text", data: "s\r" },
    ]);
    await sink.complete();
    expect(queue.idle).toBe(true);
  });

  it("preserves order between text and bytes", async () => {
    const sink = new ManualSink();
    const queue = new InputQueue(sink, vi.fn<(error: unknown) => void>());
    queue.push(text("a"));
    queue.push(bytes(0x1b, 0xff));
    queue.push(bytes(0x80));
    queue.push(text("b"));
    for (let i = 0; i < 3; i += 1) {
      // oxlint-disable-next-line no-await-in-loop -- each write completes in order
      await sink.complete();
    }
    expect(sink.calls).toEqual([
      { kind: "text", data: "a" },
      { kind: "bytes", data: [0x1b, 0xff, 0x80] },
      { kind: "text", data: "b" },
    ]);
  });

  it("reports a failed write and keeps sending later input", async () => {
    const sink = new ManualSink();
    const onError = vi.fn<(error: unknown) => void>();
    const queue = new InputQueue(sink, onError);
    sink.failNext = true;
    queue.push(text("x"));
    queue.push(text("y"));
    await sink.complete();
    expect(onError).toHaveBeenCalledOnce();
    expect(sink.calls.at(-1)).toEqual({ kind: "text", data: "y" });
  });

  it("ignores empty chunks and input after close", async () => {
    const sink = new ManualSink();
    const queue = new InputQueue(sink, vi.fn<(error: unknown) => void>());
    queue.push(text(""));
    expect(sink.calls).toEqual([]);
    queue.push(text("a"));
    queue.push(text("b"));
    queue.close();
    queue.push(text("c"));
    await sink.complete();
    expect(sink.calls).toEqual([{ kind: "text", data: "a" }]);
  });
});

describe("binaryStringToBytes", () => {
  it("maps code units to bytes without UTF-8 encoding", () => {
    expect([...binaryStringToBytes("\x1b[M\xff\x80")]).toEqual([0x1b, 0x5b, 0x4d, 0xff, 0x80]);
  });

  it("returns an empty array for an empty string", () => {
    expect(binaryStringToBytes("")).toHaveLength(0);
  });

  it("sends a fill in turn with the keys around it, never merged, and says how it went", async () => {
    const sink = new ManualSink();
    const queue = new InputQueue(sink, vi.fn<(error: unknown) => void>());
    const outcomes: string[] = [];
    queue.push(text("a"));
    queue.push({ kind: "secret", seen: 7, done: (e) => outcomes.push(e ? "failed" : "ok") });
    queue.push(text("b"));
    queue.push(text("c"));
    await sink.complete();
    await sink.complete();
    await sink.complete();
    expect(sink.calls).toEqual([text("a"), { kind: "secret", data: "7" }, text("bc")]);
    expect(outcomes).toEqual(["ok"]);
    // Dropped before it went: it hears that too.
    sink.failNext = false;
    queue.push(text("x"));
    queue.push({ kind: "secret", seen: 9, done: (e) => outcomes.push(e ? "dropped" : "ok") });
    queue.clear();
    await sink.complete();
    expect(outcomes).toEqual(["ok", "dropped"]);
  });
});
