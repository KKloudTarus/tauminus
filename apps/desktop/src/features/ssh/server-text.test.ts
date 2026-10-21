// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { serverText } from "./server-text.ts";

describe("serverText", () => {
  it("keeps printable text, newlines and tabs", () => {
    expect(serverText("  Line one\n\tLine two  ")).toBe("Line one\n\tLine two");
    expect(serverText("Mật khẩu: ")).toBe("Mật khẩu:");
  });

  it("drops control characters and escape sequences' introducers", () => {
    expect(serverText("a\u001b[31mb\u0007c\u009bd\r")).toBe("a[31mbcd");
  });

  it("drops bidirectional overrides that reorder what is shown", () => {
    expect(serverText("pass‮drow‬⁦x⁩‏")).toBe("passdrowx");
  });

  it("caps the length", () => {
    expect(serverText("x".repeat(5000))).toHaveLength(1024);
  });
});
