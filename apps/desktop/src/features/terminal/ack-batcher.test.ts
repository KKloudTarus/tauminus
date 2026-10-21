// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { ACK_THRESHOLD_BYTES, AckBatcher, BACKEND_LOW_WATERMARK_BYTES } from "./ack-batcher.ts";

describe("AckBatcher", () => {
  it("holds acknowledgements below the threshold", () => {
    const acks = new AckBatcher(10);
    expect(acks.add(4)).toBe(0);
    expect(acks.add(5)).toBe(0);
    expect(acks.pending).toBe(9);
  });

  it("releases everything pending once the threshold is reached", () => {
    const acks = new AckBatcher(10);
    acks.add(6);
    expect(acks.add(4)).toBe(10);
    expect(acks.pending).toBe(0);
  });

  it("releases an oversized batch whole", () => {
    const acks = new AckBatcher(10);
    acks.add(3);
    expect(acks.add(100)).toBe(103);
  });

  it("never loses bytes across many batches", () => {
    const acks = new AckBatcher(7);
    let released = 0;
    for (let i = 1; i <= 100; i += 1) {
      released += acks.add(i);
    }
    expect(released + acks.pending).toBe(5050);
  });

  it.each([0, -1, 1.5, Number.NaN])("rejects invalid threshold %s", (threshold) => {
    expect(() => new AckBatcher(threshold)).toThrow(RangeError);
  });

  it("default threshold stays below the backend low watermark", () => {
    expect(ACK_THRESHOLD_BYTES).toBeLessThan(BACKEND_LOW_WATERMARK_BYTES);
  });
});
