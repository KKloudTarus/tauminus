// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { MAX_SHOWN, visibleText } from "./visible-text.ts";

describe("visibleText", () => {
  it("keeps ordinary text, newlines and tabs", () => {
    expect(visibleText("rm -rf /\n\techo Máy chủ 🚀")).toBe("rm -rf /\n\techo Máy chủ 🚀");
  });

  it("shows controls and bidirectional overrides instead of obeying them", () => {
    expect(visibleText("a\u001b[31mb\u0007\u007f")).toBe("a^[[31mb^G^?");
    const rlo = String.fromCodePoint(0x202e);
    const pdf = String.fromCodePoint(0x202c);
    expect(visibleText(`ls ${rlo}rm -rf /${pdf}`)).toBe("ls <U+202E>rm -rf /<U+202C>");
    const zwsp = String.fromCodePoint(0x200b);
    expect(visibleText(`r${zwsp}m\u0085`)).toBe("r<U+200B>m<U+0085>");
    const hiddenOnes = [0xad, 0x2060, 0x3164, 0xfe0f, 0xe0041].map((c) => String.fromCodePoint(c));
    expect(visibleText(hiddenOnes.join(""))).toBe("<U+00AD><U+2060><U+3164><U+FE0F><U+E0041>");
  });

  it("summarizes text past the limit", () => {
    const text = "x".repeat(MAX_SHOWN + 10);
    expect(visibleText(text)).toBe(`${"x".repeat(MAX_SHOWN)}… 10 more characters`);
  });
});
