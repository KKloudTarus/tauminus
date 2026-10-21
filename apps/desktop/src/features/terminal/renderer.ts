// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

export type RendererPreference = "auto" | "webgl" | "dom";

/**
 * Picks the xterm.js renderer for this webview.
 *
 * On WebKitGTK (Linux), the WebGL renderer leaves the screen blank after a
 * full-screen program such as `vim` switches to the alternate buffer, until
 * something forces a redraw. The DOM renderer draws correctly there, about
 * 15% slower in our 100 MB benchmark and with less memory (docs/benchmarks.md),
 * so `auto` uses it on WebKitGTK and WebGL everywhere else.
 */
export function chooseRenderer(preference: RendererPreference, userAgent: string): "webgl" | "dom" {
  if (preference !== "auto") {
    return preference;
  }
  return isWebKitGtk(userAgent) ? "dom" : "webgl";
}

/** WebKitGTK reports X11 or Wayland Linux with AppleWebKit and no Chrome token. */
export function isWebKitGtk(userAgent: string): boolean {
  return (
    /\bLinux\b/.test(userAgent) &&
    !/\bAndroid\b/.test(userAgent) &&
    /\bAppleWebKit\//.test(userAgent) &&
    !/\b(?:Chrome|Chromium|HeadlessChrome)\//.test(userAgent)
  );
}
