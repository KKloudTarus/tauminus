// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import { WebglBudget } from "./webgl-budget.ts";

describe("WebglBudget", () => {
  it("admits holders up to capacity without evicting", () => {
    const budget = new WebglBudget(2);
    const evictA = vi.fn<() => void>();
    const evictB = vi.fn<() => void>();
    budget.acquire({}, evictA);
    budget.acquire({}, evictB);
    expect(budget.size).toBe(2);
    expect(evictA).not.toHaveBeenCalled();
    expect(evictB).not.toHaveBeenCalled();
  });

  it("evicts the least recently acquired holder when full", () => {
    const budget = new WebglBudget(2);
    const [a, b, c] = [{}, {}, {}];
    const evictA = vi.fn<() => void>();
    const evictB = vi.fn<() => void>();
    budget.acquire(a, evictA);
    budget.acquire(b, evictB);
    budget.acquire(a, evictA); // a becomes most recent
    budget.acquire(c, vi.fn<() => void>());
    expect(evictB).toHaveBeenCalledOnce();
    expect(evictA).not.toHaveBeenCalled();
    expect(budget.size).toBe(2);
  });

  it("re-acquiring a holder does not grow the budget", () => {
    const budget = new WebglBudget(1);
    const holder = {};
    const evict = vi.fn<() => void>();
    budget.acquire(holder, evict);
    budget.acquire(holder, evict);
    expect(budget.size).toBe(1);
    expect(evict).not.toHaveBeenCalled();
  });

  it("released holders free their slot and are never evicted", () => {
    const budget = new WebglBudget(1);
    const holder = {};
    const evict = vi.fn<() => void>();
    budget.acquire(holder, evict);
    budget.release(holder);
    budget.acquire({}, vi.fn<() => void>());
    expect(evict).not.toHaveBeenCalled();
  });

  it.each([0, -1, 2.5, Number.NaN])("rejects capacity %s", (capacity) => {
    expect(() => new WebglBudget(capacity)).toThrow(RangeError);
  });
});
