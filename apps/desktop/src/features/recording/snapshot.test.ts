// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Differential: a terminal written up to some point, and a fresh one given
// that point's snapshot, must then behave the same for whatever follows.
// The cases are the states the serialize addon alone gets wrong.

import { SerializeAddon } from "@xterm/addon-serialize";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";

import { CHECKPOINT_SCROLLBACK, atRest, snapshot } from "./snapshot.ts";

function terminal(cols = 40, rows = 10): { term: Terminal; serializer: SerializeAddon } {
  const term = new Terminal({
    cols,
    rows,
    scrollback: CHECKPOINT_SCROLLBACK,
    allowProposedApi: true,
  });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  const serializer = new SerializeAddon();
  term.loadAddon(serializer);
  return { term, serializer };
}

function write(term: Terminal, data: string): Promise<void> {
  return new Promise((resolve) => term.write(data, resolve));
}

/** Every cell of the active screen, with its look, and the cursor. */
function screen(term: Terminal): string {
  const buffer = term.buffer.active;
  const rows: string[] = [`${buffer.type} cursor ${buffer.cursorX},${buffer.cursorY}`];
  for (let y = 0; y < term.rows; y += 1) {
    const line = buffer.getLine(buffer.baseY + y);
    let row = "";
    for (let x = 0; x < term.cols; x += 1) {
      const cell = line?.getCell(x);
      if (!cell) continue;
      row += `${cell.getChars() || " "}:${cell.getFgColor()}/${cell.getBgColor()}/${cell.isBold()}${cell.isUnderline()}|`;
    }
    rows.push(row);
  }
  return rows.join("\n");
}

const CASES: Record<string, { before: string; after: string }> = {
  "scroll region": {
    before: "\u001b[2J\u001b[3;6r\u001b[6;1H",
    after: "a\nb\nc\nd\ne\n",
  },
  "origin mode in a region": {
    before: "\u001b[2J\u001b[4;8r\u001b[?6h",
    after: "\u001b[1;1Htop of region",
  },
  "line drawing": {
    before: "\u001b(0",
    after: "lqqqk\r\nx   x\r\nmqqqj",
  },
  "tab stops": {
    before: "\u001b[3g\u001b[1;5H\u001bH\u001b[1;17H\u001bH\r",
    after: "\ta\tb",
  },
  "saved cursor": {
    before: "\u001b[5;10H\u001b[1;31m\u001b7\u001b[0m\u001b[1;1H",
    after: "\u001b8saved",
  },
  "pending wrap above the last row": {
    before: `\u001b[3;1H${"x".repeat(40)}`,
    after: "wrapped",
  },
  "hidden cursor": {
    before: "\u001b[?25l",
    after: "still hidden",
  },
  "current colours": {
    before: "\u001b[38;5;202;48;2;1;2;3;4m",
    after: "coloured",
  },
  "alternate screen": {
    before: "main\u001b[?1049h\u001b[2Jfull screen",
    after: "\u001b[?1049lback",
  },
  "a wide character in the last column, waiting to wrap": {
    before: `\u001b[3;1H${"漢".repeat(20)}`,
    after: "next",
  },
  "the G2 set, shifted in with LS2": {
    before: "\u001b*A\u001bn",
    after: "#1",
  },
  "wide characters at the edge": {
    before: `\u001b[1;39H`,
    after: "漢字🙂",
  },
};

describe("snapshot", () => {
  for (const [name, { before, after }] of Object.entries(CASES)) {
    it(`restores ${name}`, async () => {
      const live = terminal();
      await write(live.term, `prompt$ ls\r\nfile1 file2\r\n${before}`);
      expect(atRest(live.term)).toBe(true);
      const snap = snapshot(live.term, live.serializer);
      expect(snap).toBeDefined();
      const restored = terminal();
      await write(restored.term, snap ?? "");
      await write(live.term, after);
      await write(restored.term, after);
      expect(screen(restored.term)).toBe(screen(live.term));
    });
  }

  it("takes no snapshot in the middle of an escape sequence", async () => {
    const live = terminal();
    await write(live.term, "text\u001b[");
    expect(atRest(live.term)).toBe(false);
    expect(snapshot(live.term, live.serializer)).toBeUndefined();
    await write(live.term, "31mred");
    expect(atRest(live.term)).toBe(true);
  });
});
