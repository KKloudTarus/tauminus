// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { CapturesStore, MAX_CAPTURES, SEND_EVERY_MS, SendLimiter } from "./captures-store.ts";

describe("CapturesStore", () => {
  it("keeps the newest lines first, a bounded number", () => {
    const store = new CapturesStore();
    for (let i = 0; i < MAX_CAPTURES + 5; i += 1) {
      store.add({ at: i, rule: "r", pane: "p", text: `line ${i}` });
    }
    const kept = store.getSnapshot();
    expect(kept).toHaveLength(MAX_CAPTURES);
    expect(kept[0]?.text).toBe(`line ${MAX_CAPTURES + 4}`);
    // Each its own id, so a list can key them.
    expect(new Set(kept.map((c) => c.id)).size).toBe(MAX_CAPTURES);
    store.addMany([
      { at: 1, rule: "r", pane: "p", text: "a" },
      { at: 1, rule: "r", pane: "p", text: "b" },
    ]);
    expect(
      store
        .getSnapshot()
        .slice(0, 2)
        .map((c) => c.text),
    ).toEqual(["b", "a"]);
    store.clear();
    expect(store.getSnapshot()).toHaveLength(0);
  });
});

describe("SendLimiter", () => {
  it("turns a rule off in its pane once it sends too fast, until the rules change", () => {
    const limiter = new SendLimiter();
    expect(limiter.allow("pane/yes", 0)).toBe(true);
    // Its own echo matched again at once: a loop.
    expect(limiter.allow("pane/yes", 100)).toBe("off");
    expect(limiter.allow("pane/yes", 100_000)).toBe(false);
    // Other panes and rules are their own.
    expect(limiter.allow("other/yes", 100)).toBe(true);
    limiter.reset();
    expect(limiter.allow("pane/yes", 200_000)).toBe(true);
  });

  it("allows a few sends a minute, spaced", () => {
    const limiter = new SendLimiter();
    for (let i = 0; i < 5; i += 1) expect(limiter.allow("k", i * SEND_EVERY_MS)).toBe(true);
    expect(limiter.allow("k", 5 * SEND_EVERY_MS)).toBe("off");
  });
});
