// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import {
  type Direction,
  type LayoutNode,
  type Side,
  assertValid,
  closePane,
  neighbor,
  pane,
  paneIds,
  paneReceivingSpace,
  paneRects,
  resizeSplit,
  splitPane,
} from "./layout.ts";

/** Builds a layout with ids p1, p2, ... and s1, s2, ... from a list of splits. */
function build(steps: [target: string, direction: Direction][]): LayoutNode {
  let root = pane("p1");
  steps.forEach(([target, direction], index) => {
    root = splitPane(root, target, direction, `p${index + 2}`, `s${index + 1}`);
    assertValid(root);
  });
  return root;
}

describe("splitPane", () => {
  it("turns a single pane into a split of two halves", () => {
    const root = splitPane(pane("a"), "a", "row", "b", "s");
    expect(root).toEqual({
      kind: "split",
      id: "s",
      direction: "row",
      children: [pane("a"), pane("b")],
      sizes: [50, 50],
    });
  });

  it("adds a sibling when the parent already splits that way", () => {
    const root = build([
      ["p1", "row"],
      ["p2", "row"],
    ]);
    expect(paneIds(root)).toEqual(["p1", "p2", "p3"]);
    expect(root.kind === "split" && root.sizes).toEqual([50, 25, 25]);
  });

  it("nests a split when the direction differs", () => {
    const root = build([
      ["p1", "row"],
      ["p2", "column"],
    ]);
    expect(paneIds(root)).toEqual(["p1", "p2", "p3"]);
    expect(root.kind === "split" && root.children[1]?.kind).toBe("split");
  });

  it("leaves the tree unchanged for an unknown pane", () => {
    const root = build([["p1", "row"]]);
    expect(splitPane(root, "nope", "row", "x", "y")).toBe(root);
  });
});

describe("closePane", () => {
  it("returns null when the last pane closes", () => {
    expect(closePane(pane("a"), "a")).toBeNull();
  });

  it("gives the space to the previous sibling", () => {
    const root = build([
      ["p1", "row"],
      ["p2", "row"],
    ]); // sizes 50, 25, 25
    const closed = closePane(root, "p3");
    expect(closed?.kind === "split" && closed.sizes).toEqual([50, 50]);
  });

  it("gives the first pane's space to the next sibling", () => {
    const root = build([
      ["p1", "row"],
      ["p2", "row"],
    ]);
    const closed = closePane(root, "p1");
    expect(paneIds(closed!)).toEqual(["p2", "p3"]);
    expect(closed?.kind === "split" && closed.sizes).toEqual([75, 25]);
  });

  it("collapses a split left with one child", () => {
    const root = build([["p1", "row"]]);
    expect(closePane(root, "p2")).toEqual(pane("p1"));
  });

  it("flattens a nested split that now repeats its parent's direction", () => {
    // row[p1, column[p2, row[p3, p4]]]; closing p2 leaves row[p1, row[p3, p4]].
    const root = build([
      ["p1", "row"],
      ["p2", "column"],
      ["p3", "row"],
    ]);
    const closed = closePane(root, "p2")!;
    assertValid(closed);
    expect(paneIds(closed)).toEqual(["p1", "p3", "p4"]);
    expect(closed.kind === "split" && closed.sizes).toEqual([50, 25, 25]);
  });

  it("leaves the tree unchanged for an unknown pane", () => {
    const root = build([["p1", "row"]]);
    expect(closePane(root, "nope")).toBe(root);
  });
});

describe("paneReceivingSpace", () => {
  // row[p1, column[p2, p3]]
  const root = build([
    ["p1", "row"],
    ["p2", "column"],
  ]);

  it("is the previous sibling's adjacent pane", () => {
    expect(paneReceivingSpace(root, "p3")).toBe("p2");
  });

  it("is the next sibling's first pane when the target was first", () => {
    expect(paneReceivingSpace(root, "p2")).toBe("p3");
    expect(paneReceivingSpace(root, "p1")).toBe("p2");
  });

  it("is undefined for the root pane or an unknown pane", () => {
    expect(paneReceivingSpace(pane("solo"), "solo")).toBeUndefined();
    expect(paneReceivingSpace(root, "nope")).toBeUndefined();
  });

  it("matches the pane that actually grows", () => {
    for (const target of paneIds(root)) {
      const receiver = paneReceivingSpace(root, target)!;
      const before = paneRects(root).get(receiver)!;
      const after = paneRects(closePane(root, target)!).get(receiver)!;
      expect(after.width * after.height).toBeGreaterThan(before.width * before.height);
    }
  });
});

describe("resizeSplit", () => {
  it("normalizes sizes to sum to 100", () => {
    const root = build([["p1", "row"]]);
    const resized = resizeSplit(root, "s1", [1, 3]);
    expect(resized.kind === "split" && resized.sizes).toEqual([25, 75]);
  });

  it("ignores wrong lengths and non-positive or non-finite sizes", () => {
    const root = build([["p1", "row"]]);
    expect(resizeSplit(root, "s1", [100])).toBe(root);
    expect(resizeSplit(root, "s1", [0, 100])).toBe(root);
    expect(resizeSplit(root, "s1", [Number.NaN, 1])).toBe(root);
    expect(resizeSplit(root, "s1", [Number.POSITIVE_INFINITY, 50])).toBe(root);
  });

  it("stays finite for sizes near the largest double", () => {
    const root = build([["p1", "row"]]);
    const resized = resizeSplit(root, "s1", [1e308, 1e308]);
    expect(resized.kind === "split" && resized.sizes).toEqual([50, 50]);
  });

  it("only rebuilds the path to the resized split", () => {
    const root = build([
      ["p1", "row"],
      ["p2", "column"],
    ]);
    const resized = resizeSplit(root, "s2", [30, 70]);
    expect(resized.kind === "split" && resized.children[0]).toBe(
      root.kind === "split" ? root.children[0] : undefined,
    );
  });
});

describe("paneRects and neighbor", () => {
  // ┌────┬────┐
  // │ p1 │ p2 │
  // │    ├────┤
  // │    │ p3 │
  // └────┴────┘
  const root = build([
    ["p1", "row"],
    ["p2", "column"],
  ]);

  it("tiles the unit square", () => {
    const rects = paneRects(root);
    const area = [...rects.values()].reduce((sum, r) => sum + r.width * r.height, 0);
    expect(area).toBeCloseTo(1);
    expect(rects.get("p3")).toEqual({ x: 0.5, y: 0.5, width: 0.5, height: 0.5 });
  });

  it.each<[string, Side, string | undefined]>([
    ["p1", "right", "p2"],
    ["p2", "left", "p1"],
    ["p3", "left", "p1"],
    ["p2", "down", "p3"],
    ["p3", "up", "p2"],
    ["p1", "left", undefined],
    ["p2", "up", undefined],
  ])("from %s going %s reaches %s", (from, side, expected) => {
    expect(neighbor(root, from, side)).toBe(expected);
  });

  it("prefers the candidate with the most overlap", () => {
    // p1 left; right column split 30/70: going right from p1 picks p3 (70%).
    const resized = resizeSplit(root, "s2", [30, 70]);
    expect(neighbor(resized, "p1", "right")).toBe("p3");
  });
});

/** Mulberry32: a tiny seeded PRNG, so failures reproduce. */
function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe("random operation sequences", () => {
  it("keep every invariant and the pane set across 2000 sequences", () => {
    const violations: string[] = [];
    for (let seed = 1; seed <= 2000; seed += 1) {
      const random = prng(seed);
      let root: LayoutNode | null = pane("p0");
      const expected = new Set(["p0"]);
      let next = 1;
      for (let step = 0; step < 40 && root; step += 1) {
        const ids = paneIds(root);
        const target = ids[Math.floor(random() * ids.length)] ?? "p0";
        const roll = random();
        if (roll < 0.5 && ids.length < 16) {
          const id = `p${next}`;
          root = splitPane(root, target, random() < 0.5 ? "row" : "column", id, `s${next}`);
          next += 1;
          expected.add(id);
        } else if (roll < 0.85) {
          root = closePane(root, target);
          expected.delete(target);
        } else {
          // Resize a split at any depth.
          const splits: { id: string; count: number }[] = [];
          const collect = (node: LayoutNode) => {
            if (node.kind === "split") {
              splits.push({ id: node.id, count: node.children.length });
              node.children.forEach(collect);
            }
          };
          collect(root);
          const chosen = splits[Math.floor(random() * splits.length)];
          if (chosen) {
            root = resizeSplit(
              root,
              chosen.id,
              Array.from({ length: chosen.count }, () => 0.5 + random() * 9),
            );
          }
        }
        if (root) assertValid(root);
        const actual = root ? paneIds(root) : [];
        expect(new Set(actual)).toEqual(expected);
        expect(actual).toHaveLength(expected.size);
        const area = root
          ? [...paneRects(root).values()].reduce((sum, r) => sum + r.width * r.height, 0)
          : 1;
        expect(area).toBeCloseTo(1, 9);
        if (root) violations.push(...neighborViolations(root));
      }
    }
    expect(violations).toEqual([]);
  });
});

const SIDES = ["left", "right", "up", "down"] as const;

function onBorder(r: { x: number; y: number; width: number; height: number }, side: Side): boolean {
  const eps = 1e-9;
  const border: Record<Side, boolean> = {
    left: r.x < eps,
    right: r.x + r.width > 1 - eps,
    up: r.y < eps,
    down: r.y + r.height > 1 - eps,
  };
  return border[side];
}

/**
 * Every neighbour lies on the requested side with no gap, and one is found
 * exactly when the pane's edge is not on the window border. Returns the
 * violations so the property test asserts once instead of millions of times.
 */
function neighborViolations(root: LayoutNode): string[] {
  const rects = paneRects(root);
  const found: string[] = [];
  for (const [id, r] of rects) {
    for (const side of SIDES) {
      const next = neighbor(root, id, side);
      if ((next === undefined) !== onBorder(r, side)) {
        found.push(`${id} ${side}: neighbour ${String(next)} but border ${onBorder(r, side)}`);
        continue;
      }
      const n = next ? rects.get(next) : undefined;
      if (!n) continue;
      const gaps: Record<Side, number> = {
        left: r.x - (n.x + n.width),
        right: n.x - (r.x + r.width),
        up: r.y - (n.y + n.height),
        down: n.y - (r.y + r.height),
      };
      if (Math.abs(gaps[side]) >= 1e-9) found.push(`${id} ${side}: gap ${gaps[side]} to ${next}`);
    }
  }
  return found;
}
