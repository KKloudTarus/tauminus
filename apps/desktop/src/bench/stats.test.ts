// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { FRAME_MS, FrameCounter, summarize, toHex } from "./stats.ts";

describe("FrameCounter", () => {
  it("counts the frames a long gap stands for as dropped", () => {
    const counter = new FrameCounter();
    counter.tick(0);
    counter.tick(FRAME_MS);
    counter.tick(FRAME_MS * 4);
    expect(counter.frames).toBe(2);
    expect(counter.dropped).toBe(2);
    expect(counter.droppedShare).toBe(0.5);
    expect(counter.maxGapMs).toBeCloseTo(FRAME_MS * 3);
  });

  it("drops nothing on an even pace and reports zero before any frame", () => {
    const counter = new FrameCounter();
    expect(counter.droppedShare).toBe(0);
    for (let i = 0; i < 10; i += 1) counter.tick(i * FRAME_MS * 1.2);
    expect(counter.dropped).toBe(0);
  });
});

describe("summarize", () => {
  it("takes nearest-rank percentiles and skips failed probes", () => {
    const samples = Array.from({ length: 100 }, (_, i) => i + 1);
    expect(summarize([...samples, Number.NaN])).toEqual({
      n: 100,
      p50: 50,
      p95: 95,
      p99: 99,
      max: 100,
    });
    expect(summarize([]).n).toBe(0);
  });
});

describe("toHex", () => {
  it("encodes UTF-8 bytes", () => {
    expect(toHex('{"a":"é"}')).toBe("7b2261223a22c3a9227d");
  });
});
