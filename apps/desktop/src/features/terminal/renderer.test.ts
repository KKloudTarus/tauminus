// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { chooseRenderer, isWebKitGtk } from "./renderer.ts";

const agents = {
  webkitGtk: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)",
  webkitGtkWayland: "Mozilla/5.0 (Wayland; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)",
  chromiumLinux:
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36",
  headlessChrome:
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/156.0.0.0 Safari/537.36",
  macWkWebView:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)",
  webView2:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
  androidWebView:
    "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/141.0 Mobile Safari/537.36",
};

describe("isWebKitGtk", () => {
  it.each([
    ["webkitGtk", true],
    ["webkitGtkWayland", true],
    ["chromiumLinux", false],
    ["headlessChrome", false],
    ["macWkWebView", false],
    ["webView2", false],
    ["androidWebView", false],
  ] as const)("%s -> %s", (agent, expected) => {
    expect(isWebKitGtk(agents[agent])).toBe(expected);
  });
});

describe("chooseRenderer", () => {
  it("uses the DOM renderer on WebKitGTK and WebGL elsewhere", () => {
    expect(chooseRenderer("auto", agents.webkitGtk)).toBe("dom");
    expect(chooseRenderer("auto", agents.macWkWebView)).toBe("webgl");
    expect(chooseRenderer("auto", agents.webView2)).toBe("webgl");
  });

  it("honours an explicit preference", () => {
    expect(chooseRenderer("webgl", agents.webkitGtk)).toBe("webgl");
    expect(chooseRenderer("dom", agents.webView2)).toBe("dom");
  });
});
