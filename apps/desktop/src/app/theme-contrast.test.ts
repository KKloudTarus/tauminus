// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The color tokens in styles.css, checked against WCAG 2.2 contrast ratios.
// Each theme is written twice (light as :root and .light, dark inside the
// media query and as .dark); the copies must match.

import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

/** The custom properties declared in the first block that opens with `selector {`. */
function block(selector: string): Map<string, string> {
  const start = css.indexOf(`${selector} {`);
  if (start < 0) throw new Error(`styles.css has no ${selector} block`);
  const end = css.indexOf("}", start);
  const vars = new Map<string, string>();
  for (const match of css.slice(start, end).matchAll(/(--[\w-]+):\s*([^;]+);/gu)) {
    vars.set(match[1] ?? "", (match[2] ?? "").trim());
  }
  return vars;
}

type Rgba = [number, number, number, number];

function parse(color: string): Rgba {
  const hex = /^#([\da-f]{6})$/iu.exec(color);
  if (hex) {
    const n = Number.parseInt(hex[1] ?? "", 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 1];
  }
  const rgb = /^rgb\((\d+) (\d+) (\d+)(?: \/ ([\d.]+))?\)$/u.exec(color);
  if (rgb) return [Number(rgb[1]), Number(rgb[2]), Number(rgb[3]), Number(rgb[4] ?? 1)];
  throw new Error(`unsupported color ${color}`);
}

/** A translucent color painted over an opaque one. */
function over([r, g, b, a]: Rgba, [br, bg, bb]: Rgba): Rgba {
  return [r * a + br * (1 - a), g * a + bg * (1 - a), b * a + bb * (1 - a), 1];
}

function channel(v: number): number {
  const s = v / 255;
  return s <= 0.040_45 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function luminance([r, g, b]: Rgba): number {
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrast(fg: Rgba, bg: Rgba): number {
  const [a, b] = [luminance(fg), luminance(bg)];
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

const ANSI = ["red", "green", "yellow", "blue", "magenta", "cyan", "white"];

test("each theme's two copies match", () => {
  const dark = block(".dark");
  const darkMedia = block(":root:not(.light)");
  const light = block(".light");
  const root = block(":root");
  expect(Object.fromEntries(darkMedia)).toEqual(Object.fromEntries(dark));
  expect(Object.fromEntries(root)).toEqual(Object.fromEntries(light));
  expect([...dark.keys()].toSorted()).toEqual([...light.keys()].toSorted());
});

describe.each([".dark", ".light"])("%s", (selector) => {
  const vars = block(selector);
  const color = (name: string): Rgba => parse(vars.get(name) ?? "missing");
  const solid = (name: string, base: string): Rgba => over(color(name), color(base));

  test.each(["--background", "--surface-1", "--surface-2", "--surface-3"])(
    "text levels read on %s",
    (surface) => {
      expect(contrast(color("--foreground"), color(surface))).toBeGreaterThanOrEqual(7);
      expect(contrast(color("--muted"), color(surface))).toBeGreaterThanOrEqual(4.5);
      // Faint text is still text: host addresses, hints, timestamps.
      expect(contrast(color("--faint"), color(surface))).toBeGreaterThanOrEqual(4.5);
    },
  );

  /** The names whose color falls under `min` against `bg`; empty when all pass. */
  const below = (names: readonly string[], bg: Rgba, min: number) =>
    names.filter((name) => contrast(color(name), bg) < min);

  test("state colors read on raised surfaces", () => {
    const states = ["--accent", "--danger", "--warn", "--ok", "--prod"];
    expect(below(states, color("--surface-3"), 4.5)).toEqual([]);
    expect(contrast(color("--accent-fg"), color("--accent"))).toBeGreaterThanOrEqual(4.5);
    const tint = solid("--prod-soft", "--surface-1");
    expect(contrast(color("--prod"), tint)).toBeGreaterThanOrEqual(4.5);
    // PROD chips, the prod status bar, and the guard's Run button: white on the fill.
    expect(contrast(parse("#ffffff"), color("--prod-fill"))).toBeGreaterThanOrEqual(4.5);
  });

  test("terminal text and ANSI colors read on the terminal background", () => {
    const bg = color("--terminal-background");
    expect(contrast(color("--terminal-foreground"), bg)).toBeGreaterThanOrEqual(7);
    expect(contrast(color("--terminal-cursor"), bg)).toBeGreaterThanOrEqual(3);
    const ansi = ANSI.flatMap((name) => [`--terminal-${name}`, `--terminal-bright-${name}`]);
    expect(below(ansi, bg, 4.5)).toEqual([]);
    // Bright black is the "dim" color, for comments and suggestions: 3:1 like large text.
    expect(contrast(color("--terminal-bright-black"), bg)).toBeGreaterThanOrEqual(3);
  });

  test("selected text keeps its contrast", () => {
    const selection = color("--terminal-selection");
    expect(contrast(color("--terminal-foreground"), selection)).toBeGreaterThanOrEqual(4.5);
  });
});
