// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Terminal } from "@xterm/headless";
import { describe, expect, it, vi } from "vitest";

import type { FiredView, PreparedView, TriggerView } from "@/ipc/index.ts";

import { MAX_ROWS, type TriggerSource, TriggerWatcher } from "./trigger-watcher.ts";

const NOTIFY: TriggerView = { name: "error", action: { kind: "notify" } };
const SEND: TriggerView = { name: "answer", action: { kind: "send", text: "y", enter: true } };

function setup(options: { triggers?: TriggerView[]; rows?: number } = {}) {
  const term = new Terminal({
    cols: 40,
    rows: options.rows ?? 5,
    scrollback: 2000,
    allowProposedApi: true,
  });
  const sent: string[][] = [];
  let prepares = 0;
  let stale = false;
  const source: TriggerSource = {
    prepare: () => {
      prepares += 1;
      return Promise.resolve<PreparedView>({
        handle: prepares,
        styles: [],
        triggers: options.triggers ?? [NOTIFY],
      });
    },
    fire: (_handle, lines) => {
      if (stale) {
        stale = false;
        return Promise.resolve(null);
      }
      sent.push(lines);
      const fired: FiredView[] = [];
      lines.forEach((text, line) => {
        const at = text.indexOf("ERROR");
        if (at >= 0) fired.push({ line, rule: 0, start: at, end: at + 5, text });
      });
      return Promise.resolve(fired);
    },
  };
  const fired: string[] = [];
  const watcher = new TriggerWatcher(term, source, ({ fired: batch }) => {
    for (const one of batch) fired.push(one.text);
  });
  const write = (text: string) => new Promise<void>((resolve) => term.write(text, resolve));
  return {
    term,
    watcher,
    sent,
    fired,
    write,
    prepares: () => prepares,
    goStale: () => {
      stale = true;
    },
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 160));

describe("TriggerWatcher", () => {
  it("fires once for each finished line, and waits for the one being written", async () => {
    const { watcher, fired, write } = setup();
    await write("an ERROR here\r\nfine\r\nERROR in progress");
    await settle();
    expect(fired).toEqual(["an ERROR here"]);
    // The cursor leaves the line: now it is finished.
    await write(", done\r\n$ ");
    await settle();
    expect(fired).toEqual(["an ERROR here", "ERROR in progress, done"]);
    // Redrawing the screen does not fire again.
    await write("\r\n");
    await settle();
    expect(fired).toHaveLength(2);
    watcher.dispose();
  });

  it("starts at the cursor: what the pane showed before is not news", async () => {
    const term = new Terminal({ cols: 40, rows: 5, allowProposedApi: true });
    await new Promise<void>((resolve) => term.write("old ERROR\r\n", resolve));
    const seen: string[] = [];
    const watcher = new TriggerWatcher(
      term,
      {
        prepare: () => Promise.resolve({ handle: 1, styles: [], triggers: [NOTIFY] }),
        fire: (_handle, lines) => {
          seen.push(...lines);
          return Promise.resolve([]);
        },
      },
      () => {},
    );
    await new Promise<void>((resolve) => term.write("new ERROR\r\n", resolve));
    await settle();
    expect(seen).toEqual(["new ERROR"]);
    watcher.dispose();
  });

  it("joins a wrapped line, keeping the spaces at the wrap", async () => {
    const { watcher, fired, write } = setup();
    // 40 columns: the space at column 40 is part of the line.
    await write(`${"x".repeat(39)} ERROR wrapped\r\n`);
    await settle();
    expect(fired).toEqual([`${"x".repeat(39)} ERROR wrapped`]);
    watcher.dispose();
  });

  it("skips full-screen programs", async () => {
    const { watcher, fired, write } = setup();
    await write("\u001b[?1049hERROR in vim\r\nERROR again\r\n");
    await settle();
    expect(fired).toEqual([]);
    await write("\u001b[?1049lback\r\nERROR after\r\n");
    await settle();
    expect(fired).toEqual(["ERROR after"]);
    watcher.dispose();
  });

  it("reads at most a bounded number of rows a scan, the newest", async () => {
    const { watcher, sent, write } = setup();
    await write(Array.from({ length: MAX_ROWS * 2 }, (_, i) => `line ${i}`).join("\r\n") + "\r\n");
    await settle();
    expect(sent.flat()).toHaveLength(MAX_ROWS);
    expect(sent.flat().at(-1)).toBe(`line ${MAX_ROWS * 2 - 1}`);
    watcher.dispose();
  });

  it("with no trigger, sends nothing and does not fire old lines once rules come", async () => {
    const empty = setup({ triggers: [] });
    await empty.write("ERROR before rules\r\n");
    await settle();
    expect(empty.sent).toEqual([]);
    empty.watcher.dispose();
  });

  it("fires the same lines with the new rules when the old went stale", async () => {
    const { watcher, fired, write, goStale, prepares } = setup();
    goStale();
    await write("ERROR once\r\n");
    await settle();
    expect(prepares()).toBe(2);
    expect(fired).toEqual(["ERROR once"]);
    watcher.dispose();
  });

  it("does not prepare again after a failure until the rules change", async () => {
    const term = new Terminal({ cols: 40, rows: 5, allowProposedApi: true });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let prepares = 0;
    const watcher = new TriggerWatcher(
      term,
      {
        prepare: () => {
          prepares += 1;
          return Promise.reject(new Error("too large"));
        },
        fire: () => Promise.resolve([]),
      },
      () => {},
    );
    for (let i = 0; i < 3; i += 1) {
      // oxlint-disable-next-line no-await-in-loop -- output keeps coming
      await new Promise<void>((resolve) => term.write(`line ${i}\r\n`, resolve));
      // oxlint-disable-next-line no-await-in-loop -- output keeps coming
      await settle();
    }
    expect(prepares).toBe(1);
    watcher.refresh();
    await settle();
    expect(prepares).toBe(2);
    watcher.dispose();
    warn.mockRestore();
  });

  it("does not fire old lines again when the screen is erased", async () => {
    const { watcher, fired, write } = setup();
    await write(Array.from({ length: 12 }, (_, i) => `ERROR old ${i}`).join("\r\n") + "\r\n$ ");
    await settle();
    expect(fired).toHaveLength(12);
    // Ctrl+L: the cursor home and the screen erased, which takes the mark with it.
    await write("\u001b[H\u001b[2J$ ls\r\nfile\r\n$ ");
    await settle();
    expect(fired).toHaveLength(12);
    watcher.dispose();
  });

  it("reads the line a question waits on for send rules, once, after output stops", async () => {
    const { watcher, sent, fired, write } = setup({ triggers: [SEND] });
    await write("Continue? [y/N] ");
    await new Promise((resolve) => setTimeout(resolve, 450));
    expect(sent).toEqual([["Continue? [y/N] "]]);
    // Nothing new on the line: not read again.
    await write("");
    await new Promise((resolve) => setTimeout(resolve, 450));
    expect(sent).toHaveLength(1);
    // Answered, the question is a finished line, which send rules do not read.
    await write("n\r\n$ ");
    await new Promise((resolve) => setTimeout(resolve, 450));
    expect(sent.flat()).not.toContain("Continue? [y/N] n");
    expect(fired).toEqual([]);
    watcher.dispose();
  });
});
