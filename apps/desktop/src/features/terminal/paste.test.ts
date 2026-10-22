// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { defusePaste } from "./paste.ts";

describe("defusePaste", () => {
  const start = "\u001b[200~";
  const end = "\u001b[201~";

  it("removes every escape from inside a bracketed paste", () => {
    expect(defusePaste(`${start}echo a${end}rm -rf /\r${end}`)).toBe(
      `${start}echo a[201~rm -rf /\r${end}`,
    );
    // Removing a marker once would let these close up into a new one.
    for (const nested of ["\u001b[20\u001b[201~1~", "\u001b[20\u001b[200~1~"]) {
      const defused = defusePaste(`${start}echo a${nested} rm -rf ~\r${end}`);
      expect(defused.slice(start.length, -end.length)).not.toContain("\u001b");
    }
    expect(defusePaste(`${start}a\u009bb${end}`)).toBe(`${start}ab${end}`);
  });

  it("leaves ordinary pastes and other input alone", () => {
    expect(defusePaste(`${start}ls${end}`)).toBe(`${start}ls${end}`);
    expect(defusePaste("plain\r")).toBe("plain\r");
    expect(defusePaste("\u001b[A")).toBe("\u001b[A");
  });
});
