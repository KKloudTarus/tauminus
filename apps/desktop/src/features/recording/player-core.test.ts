// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";

import { Cast, OUTPUT, RESIZE } from "./cast.ts";
import { PlayerCore, decodeCheckpoints, encodeCheckpoints } from "./player-core.ts";
import { SNAPSHOT_FORMAT } from "./snapshot.ts";

const encoder = new TextEncoder();

/** A cast of a session in a shell, a full-screen program, and back. */
function sampleCast(rounds: number): Uint8Array {
  const lines = ['{"version":2,"width":60,"height":12,"timestamp":1700000000,"title":"t"}'];
  let time = 0;
  const out = (data: string) => {
    time += 0.01;
    lines.push(JSON.stringify([Number(time.toFixed(6)), "o", data]));
  };
  for (let r = 0; r < rounds; r += 1) {
    out(`$ ls --color\r\n\u001b[1;34mdir${r}\u001b[0m  file${r}.txt  漢字 🙂\r\n`);
    out(`\u001b[?1049h\u001b[2J\u001b[2;10r\u001b(0lqqqk\u001b(B\u001b[5;5H top ${r}`);
    // An escape sequence cut across two events.
    out("\u001b[3");
    out("1mred\u001b[0m");
    out(`\u001b[r\u001b[?1049l${"y\r\n".repeat(30)}`);
    if (r % 7 === 3) {
      time += 0.01;
      lines.push(JSON.stringify([Number(time.toFixed(6)), "r", r % 2 ? "70x14" : "60x12"]));
    }
  }
  return encoder.encode(`${lines.join("\n")}\n`);
}

function replay(cast: Cast, upTo: number): Promise<Terminal> {
  const term = new Terminal({
    cols: cast.header.width,
    rows: cast.header.height,
    scrollback: 1000,
    allowProposedApi: true,
  });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  const decoder = new TextDecoder();
  let data = "";
  for (let i = 0; i < upTo; i += 1) {
    if (cast.kinds[i] === OUTPUT) data += cast.data(i, decoder);
    else if (cast.kinds[i] === RESIZE) {
      const text = data;
      data = "";
      term.write(text);
      const [cols, rows] = cast.data(i, decoder).split("x").map(Number);
      term.resize(cols ?? 60, rows ?? 12);
    }
  }
  return new Promise((resolve) => term.write(data, () => resolve(term)));
}

function screen(term: Terminal): string {
  const buffer = term.buffer.active;
  const rows = [
    `${buffer.type} ${term.cols}x${term.rows} cursor ${buffer.cursorX},${buffer.cursorY}`,
  ];
  for (let y = 0; y < term.rows; y += 1) {
    const line = buffer.getLine(buffer.baseY + y);
    let row = "";
    for (let x = 0; x < term.cols; x += 1) {
      const cell = line?.getCell(x);
      row += `${cell?.getChars() || " "}${cell?.getFgColor() ?? ""}`;
    }
    rows.push(row);
  }
  return rows.join("\n");
}

async function shown(frame: { cols: number; rows: number; snapshot: string }): Promise<Terminal> {
  const term = new Terminal({
    cols: frame.cols,
    rows: frame.rows,
    scrollback: 1000,
    allowProposedApi: true,
  });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  await new Promise<void>((resolve) => term.write(frame.snapshot, resolve));
  return term;
}

describe("PlayerCore", () => {
  it("seeks anywhere, back and forth, to what a replay from the start shows", async () => {
    const bytes = sampleCast(60);
    // Checkpoints even for this small cast.
    const core = new PlayerCore(bytes, undefined, { small: 0, maxBytes: 2000 });
    await core.build(() => {});
    expect(core.checkpoints.length).toBeGreaterThan(2);
    const count = core.cast.count;
    const targets = [count, 3, Math.floor(count / 2), 1, count - 7, 4, Math.floor(count / 3), 0];
    for (const target of targets) {
      // oxlint-disable-next-line no-await-in-loop -- one seek after the other
      const frame = await core.seek(target);
      expect(frame.event).toBeGreaterThanOrEqual(target);
      // oxlint-disable-next-line no-await-in-loop -- compared in turn
      const [restored, expected] = await Promise.all([
        shown(frame),
        replay(core.cast, frame.event),
      ]);
      expect(screen(restored), `seek to ${target}`).toBe(screen(expected));
    }
  });

  it("keeps checkpoints that match its format, and rebuilds others", async () => {
    const bytes = sampleCast(20);
    const first = new PlayerCore(bytes, undefined, { small: 0, maxBytes: 2000 });
    await first.build(() => {});
    const saved = encodeCheckpoints(first.checkpoints);
    expect(decodeCheckpoints(saved, first.cast.count)).toHaveLength(first.checkpoints.length);
    const second = new PlayerCore(bytes, saved, { small: 0 });
    expect(second.progress).toBe(1);
    expect(second.builtHere).toBe(false);
    expect(
      decodeCheckpoints(saved.replace(SNAPSHOT_FORMAT, "other/1"), first.cast.count),
    ).toBeUndefined();
    expect(decodeCheckpoints("{", 10)).toBeUndefined();
    // Checkpoints past the end of this recording are not its.
    expect(decodeCheckpoints(saved, 1)).toBeUndefined();
  });

  it("finds text across escape sequences and event boundaries", async () => {
    const cast = [
      '{"version":2,"width":40,"height":5}',
      JSON.stringify([0.5, "o", "error: \u001b[31mdisk"]),
      JSON.stringify([1.0, "o", " full\u001b[0m\r\n"]),
      JSON.stringify([2.0, "o", "all good\r\n"]),
    ].join("\n");
    const core = new PlayerCore(encoder.encode(cast));
    const hits = (await core.search("DISK FULL")) ?? [];
    expect(hits.map((h) => h.time)).toEqual([1]);
    expect(hits[0]?.preview).toContain("disk full");
    expect(((await core.search("good")) ?? []).map((h) => h.event)).toEqual([2]);
    expect(await core.search("")).toEqual([]);
  });

  it("reads a cast with lines that are not events, and a resize", () => {
    const cast = Cast.parse(
      encoder.encode(
        '{"version":2,"width":80,"height":24,"title":"x"}\n[0.1, "o", "a"]\nnot json\n[0.2, "i", "typed"]\n[0.3, "r", "100x30"]\n[0.4, "o", "b"]',
      ),
    );
    expect(cast.count).toBe(3);
    expect(Array.from(cast.kinds)).toEqual([OUTPUT, RESIZE, OUTPUT]);
    expect(cast.indexAt(0.25)).toBe(1);
    expect(cast.indexAt(10)).toBe(3);
    expect(cast.duration).toBeCloseTo(0.4);
    expect(() => Cast.parse(encoder.encode('{"version":1}'))).toThrow(/v2/u);
  });

  it("plays a repeat attack the live pane drops without hanging", async () => {
    const cast = [
      '{"version":2,"width":40,"height":5}',
      JSON.stringify([0.5, "o", "A\u001b[2147483647b after"]),
    ].join("\n");
    const core = new PlayerCore(encoder.encode(cast), undefined, { small: 0 });
    await core.build(() => {});
    const frame = await core.seek(core.cast.count);
    const term = await shown(frame);
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe("A after");
  });

  it("refuses saved checkpoints with sizes no terminal has", () => {
    const forged = JSON.stringify({
      format: "tauminus-snapshot/1 xterm-6.0.0 unicode-11 sb-1000",
      checkpoints: [{ event: 1, cols: 1e9, rows: 10, data: "" }],
    });
    expect(decodeCheckpoints(forged, 5)).toBeUndefined();
  });

  it("shows the screen of a recording that ends in the middle of a sequence", async () => {
    const cast = [
      '{"version":2,"width":20,"height":4}',
      JSON.stringify([0.1, "o", "hello\r\nworld"]),
      JSON.stringify([0.2, "o", "\u001b[3"]),
    ].join("\n");
    const core = new PlayerCore(encoder.encode(cast), undefined, { small: 0 });
    await core.build(() => {});
    const term = await shown(await core.seek(core.cast.count));
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe("hello");
    expect(term.buffer.active.getLine(1)?.translateToString(true)).toBe("world");
  });

  it("plays what is whole of a recording whose last line a crash tore", async () => {
    const cast = `{"version":2,"width":20,"height":4}\n${JSON.stringify([0.1, "o", "whole"])}\n[0.2, "o", "torn`;
    const core = new PlayerCore(encoder.encode(cast), undefined, { small: 0 });
    await core.build(() => {});
    const term = await shown(await core.seek(core.cast.count));
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe("whole");
  });

  it("keeps checkpoints to a share of the recording, however dense its colour", async () => {
    // Every cell a different truecolour: the heaviest snapshots there are.
    const line = Array.from({ length: 120 }, (_, i) => `\u001b[38;2;${i};${255 - i};9mx`).join("");
    const lines = ['{"version":2,"width":120,"height":30}'];
    for (let i = 0; i < 300; i += 1) lines.push(JSON.stringify([i / 100, "o", `${line}\r\n`]));
    const bytes = encoder.encode(lines.join("\n"));
    const weigh = async (weight: number) => {
      const core = new PlayerCore(bytes, undefined, { small: 0, parseMs: 0, weight });
      await core.build(() => {});
      const sizes = core.checkpoints.map((c) => c.data.length);
      return { total: sizes.reduce((n, size) => n + size, 0), largest: Math.max(...sizes) };
    };
    // Each checkpoint but the last waits for four times its size in output.
    const spaced = await weigh(4);
    expect(spaced.total).toBeLessThanOrEqual(bytes.length / 4 + spaced.largest);
    // Spaced by parse time alone, they outweigh the recording.
    expect((await weigh(0)).total).toBeGreaterThan(bytes.length);
  }, 60_000);
});
