// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Terminal } from "@xterm/headless";
import { describe, expect, it, vi } from "vitest";

import type { HitView, PreparedView } from "@/ipc/index.ts";

import {
  type HighlightSource,
  type HighlightTerminal,
  Highlighter,
  colourOf,
} from "./highlighter.ts";

describe("colourOf", () => {
  it("takes the theme's palette colour, or xterm.js's when the theme sets none", () => {
    expect(colourOf("1", { red: "#ff0000" })).toBe("#ff0000");
    expect(colourOf("1", undefined)).toBe("#cc0000");
    expect(colourOf("15", {})).toBe("#eeeeec");
    // A theme colour in another form cannot go on a decoration: the default stays.
    expect(colourOf("1", { red: "rgb(255, 0, 0)" })).toBe("#cc0000");
  });

  it("keeps an exact colour and refuses anything else", () => {
    expect(colourOf("#12abEF", undefined)).toBe("#12abEF");
    for (const bad of ["16", "-1", "#123", "red", "", null, undefined]) {
      expect(colourOf(bad, undefined)).toBeUndefined();
    }
  });
});

/** A headless terminal whose decorations and markers are counted while alive. */
function counted(cols = 40, rows = 5) {
  const term = new Terminal({ cols, rows, allowProposedApi: true, scrollback: 50 });
  const live = { decorations: 0, markers: 0, made: 0 };
  const view: HighlightTerminal = {
    buffer: term.buffer,
    get cols() {
      return term.cols;
    },
    get rows() {
      return term.rows;
    },
    options: term.options,
    element: undefined,
    onWriteParsed: (listener) => term.onWriteParsed(listener),
    onScroll: (listener) => term.onScroll(listener),
    onResize: (listener) => term.onResize(listener),
    registerMarker: (offset) => {
      const marker = term.registerMarker(offset);
      if (!marker) return marker;
      live.markers += 1;
      marker.onDispose(() => {
        live.markers -= 1;
      });
      return marker;
    },
    registerDecoration: () => {
      live.decorations += 1;
      live.made += 1;
      let disposed = false;
      return {
        dispose: () => {
          if (!disposed) live.decorations -= 1;
          disposed = true;
        },
      };
    },
  };
  const write = (text: string) => new Promise<void>((resolve) => term.write(text, resolve));
  return { term, view, live, write };
}

// The terminal's own writes run on timers: real time, past one scan.
const settle = () => new Promise((resolve) => setTimeout(resolve, 160));

const STYLE = { fg: "1", bg: null, line: false };

function source(overrides: Partial<HighlightSource> = {}) {
  const calls = { prepare: 0, scan: 0 };
  const base: HighlightSource = {
    prepare: () => {
      calls.prepare += 1;
      return Promise.resolve<PreparedView>({ handle: calls.prepare, styles: [STYLE] });
    },
    scan: (_handle, lines) => {
      calls.scan += 1;
      const hits: HitView[] = [];
      lines.forEach((text, line) => {
        const at = text.indexOf("ERROR");
        if (at >= 0) hits.push({ line, style: 0, start: at, end: at + 5 });
      });
      return Promise.resolve(hits);
    },
  };
  return { calls, source: { ...base, ...overrides } };
}

describe("Highlighter", () => {
  it("draws what matches and gives back every decoration and marker", async () => {
    const { view, live, write } = counted();
    const { source: rules } = source();
    const highlighter = new Highlighter(view, rules);
    await write("an ERROR\r\nfine\r\nERROR again");
    await settle();
    expect(highlighter.held).toEqual({ decorations: 2, markers: 2 });
    expect(live).toMatchObject({ decorations: 2, markers: 2 });
    highlighter.dispose();
    expect(live).toMatchObject({ decorations: 0, markers: 0 });
  });

  it("leaves nothing behind when scans fail", async () => {
    const { view, live, write } = counted();
    const { source: rules } = source({ scan: () => Promise.reject(new Error("too much text")) });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const highlighter = new Highlighter(view, rules);
    for (let i = 0; i < 5; i += 1) {
      // oxlint-disable-next-line no-await-in-loop -- one failed scan after another
      await write(`ERROR ${i}\r\n`);
      // oxlint-disable-next-line no-await-in-loop -- one failed scan after another
      await settle();
    }
    expect(live).toMatchObject({ decorations: 0, markers: 0 });
    highlighter.dispose();
    warn.mockRestore();
  });

  it("prepares again when its rules went stale", async () => {
    const { view, write } = counted();
    const { calls, source: base } = source();
    let stale = true;
    const highlighter = new Highlighter(view, {
      ...base,
      scan: (handle, lines) => {
        if (stale) {
          stale = false;
          return Promise.resolve(null);
        }
        return base.scan(handle, lines);
      },
    });
    await write("ERROR");
    await settle();
    await settle();
    expect(calls.prepare).toBe(2);
    expect(highlighter.held.decorations).toBe(1);
    highlighter.dispose();
  });

  it("drops an answer to a prepare from before a refresh", async () => {
    const { view, write } = counted();
    const answers: ((prepared: PreparedView) => void)[] = [];
    const scanned: number[] = [];
    const highlighter = new Highlighter(view, {
      prepare: () => new Promise((resolve) => answers.push(resolve)),
      scan: (handle) => {
        scanned.push(handle);
        return Promise.resolve([]);
      },
    });
    await write("ERROR");
    await settle();
    expect(answers).toHaveLength(1);
    highlighter.refresh();
    // The old sets answer late: not used.
    answers[0]?.({ handle: 1, styles: [STYLE] });
    await settle();
    answers[1]?.({ handle: 2, styles: [STYLE] });
    await settle();
    expect(scanned.every((handle) => handle === 2)).toBe(true);
    expect(scanned.length).toBeGreaterThan(0);
    highlighter.dispose();
  });

  it("does not prepare again after a failure until its rules change", async () => {
    const { view, write } = counted();
    let prepares = 0;
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const highlighter = new Highlighter(view, {
      prepare: () => {
        prepares += 1;
        return Promise.reject(new Error("too large"));
      },
      scan: () => Promise.resolve([]),
    });
    for (let i = 0; i < 5; i += 1) {
      // oxlint-disable-next-line no-await-in-loop -- output keeps coming
      await write(`line ${i}\r\n`);
      // oxlint-disable-next-line no-await-in-loop -- output keeps coming
      await settle();
    }
    expect(prepares).toBe(1);
    highlighter.refresh();
    await settle();
    expect(prepares).toBe(2);
    highlighter.dispose();
    warn.mockRestore();
  });

  it("clears what it drew when the pane switches to the other screen", async () => {
    const { view, live, write } = counted();
    const scans: (() => void)[] = [];
    const { source: base } = source();
    const highlighter = new Highlighter(view, base);
    await write("ERROR");
    await settle();
    expect(live.decorations).toBe(1);
    // A full-screen program starts: nothing from the normal screen stays.
    await write("\u001b[?1049h");
    expect(live.decorations).toBe(0);
    expect(scans).toHaveLength(0);
    highlighter.dispose();
  });

  it("keeps what it drew where the next scan finds the same", async () => {
    const { view, live, write } = counted();
    const { source: rules } = source();
    const highlighter = new Highlighter(view, rules);
    await write("an ERROR\r\n");
    await settle();
    expect(live.made).toBe(1);
    // New output below: the old match is still there, and stays drawn.
    await write("more\r\nand an ERROR\r\n");
    await settle();
    expect(live).toMatchObject({ decorations: 2, made: 2 });
    highlighter.dispose();
  });
});
