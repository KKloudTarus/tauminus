// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { asksToOpen, isOpenableLink } from "./links.ts";

describe("isOpenableLink", () => {
  it.each(["https://example.com", "http://example.com/a?b=1#c", "HTTPS://EXAMPLE.COM"])(
    "opens %s",
    (url) => expect(isOpenableLink(url)).toBe(true),
  );

  it.each([
    "file:///etc/passwd",
    "javascript:alert(1)",
    "https://",
    "https:///x",
    "https://exa mple.com",
    "https://exаmple.com",
    `https://example.com/${"a".repeat(2048)}`,
  ])("refuses %s", (url) => expect(isOpenableLink(url)).toBe(false));
});

const click = (keys: { ctrlKey?: boolean; metaKey?: boolean }) => ({
  ctrlKey: false,
  metaKey: false,
  ...keys,
});

describe("asksToOpen", () => {
  it("needs Ctrl, or Cmd on macOS", () => {
    expect(asksToOpen(click({ ctrlKey: true }), false)).toBe(true);
    expect(asksToOpen(click({}), false)).toBe(false);
    expect(asksToOpen(click({ metaKey: true }), true)).toBe(true);
    expect(asksToOpen(click({ ctrlKey: true }), true)).toBe(false);
  });
});
