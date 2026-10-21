// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { dropsRepeat } from "./repeat-guard.ts";

describe("dropsRepeat", () => {
  it("keeps repeats up to one screenful", () => {
    expect(dropsRepeat([80], 80, 24)).toBe(false);
    expect(dropsRepeat([80 * 24], 80, 24)).toBe(false);
  });

  it("drops a repeat longer than the screen", () => {
    expect(dropsRepeat([80 * 24 + 1], 80, 24)).toBe(true);
    expect(dropsRepeat([2_147_483_647], 80, 24)).toBe(true);
  });

  it("treats a missing or zero count as one, as xterm.js does", () => {
    expect(dropsRepeat([], 1, 1)).toBe(false);
    expect(dropsRepeat([0], 1, 1)).toBe(false);
    expect(dropsRepeat([[5, 2]], 1, 1)).toBe(false);
  });
});
