// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The split-pane layout of one tab, as an immutable tree.
 *
 * Invariants, checked by `assertValid` in tests after every operation:
 * 1. A split has at least two children.
 * 2. `sizes` has one entry per child, each positive, summing to 100.
 * 3. No split has a child split with the same direction; such children are
 *    flattened into their parent, so every layout has one canonical shape.
 * 4. Node ids are unique within a tree.
 *
 * Every operation walks the tree once: O(n) time for n nodes, rebuilding only
 * the path to the changed node. A tab holds at most 16 panes, so n <= 31.
 */

export type PaneId = string;

/** `row` places children left to right, `column` top to bottom. */
export type Direction = "row" | "column";

export type LayoutNode =
  | { readonly kind: "pane"; readonly id: PaneId }
  | {
      readonly kind: "split";
      readonly id: string;
      readonly direction: Direction;
      readonly children: readonly LayoutNode[];
      readonly sizes: readonly number[];
    };

export type Side = "left" | "right" | "up" | "down";

export interface Rect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

const TOTAL = 100;

export function pane(id: PaneId): LayoutNode {
  return { kind: "pane", id };
}

/** Pane ids in reading order: left to right, top to bottom. O(n). */
export function paneIds(root: LayoutNode): PaneId[] {
  const ids: PaneId[] = [];
  const visit = (node: LayoutNode) => {
    if (node.kind === "pane") {
      ids.push(node.id);
    } else {
      node.children.forEach(visit);
    }
  };
  visit(root);
  return ids;
}

export function countPanes(root: LayoutNode): number {
  return paneIds(root).length;
}

/**
 * Splits `target` and places pane `created` after it, along `direction`.
 * The target's space is shared equally with the new pane; siblings keep
 * their sizes. Returns the tree unchanged when `target` is not in it.
 */
export function splitPane(
  root: LayoutNode,
  target: PaneId,
  direction: Direction,
  created: PaneId,
  splitId: string,
): LayoutNode {
  const replaced = mapPane(root, target, (leaf, parent) => {
    if (parent?.direction === direction) {
      return [leaf, pane(created)];
    }
    return [
      {
        kind: "split",
        id: splitId,
        direction,
        children: [leaf, pane(created)],
        sizes: [TOTAL / 2, TOTAL / 2],
      },
    ];
  });
  return replaced ?? root;
}

/**
 * Removes pane `target`. Its space goes to the previous sibling, or to the
 * next one when it was first. Returns `null` when the last pane is removed,
 * and the tree unchanged when `target` is not in it.
 */
export function closePane(root: LayoutNode, target: PaneId): LayoutNode | null {
  if (root.kind === "pane") {
    return root.id === target ? null : root;
  }
  const replaced = mapPane(root, target, () => []);
  return replaced ?? root;
}

/** Every split's children the same size, as a grid opened at once wants. O(n). */
export function balanced(root: LayoutNode): LayoutNode {
  if (root.kind === "pane") return root;
  return {
    ...root,
    children: root.children.map(balanced),
    sizes: root.children.map(() => TOTAL / root.children.length),
  };
}

/** Sets a split's sizes, normalized to sum to 100. Unknown ids leave the tree as is. */
export function resizeSplit(
  root: LayoutNode,
  splitId: string,
  sizes: readonly number[],
): LayoutNode {
  if (root.kind === "pane") {
    return root;
  }
  if (root.id === splitId) {
    if (
      sizes.length !== root.children.length ||
      sizes.some((s) => !(Number.isFinite(s) && s > 0))
    ) {
      return root;
    }
    return { ...root, sizes: normalize(sizes) };
  }
  let changed = false;
  const children = root.children.map((child) => {
    const next = resizeSplit(child, splitId, sizes);
    changed ||= next !== child;
    return next;
  });
  return changed ? { ...root, children } : root;
}

/**
 * The pane that receives the space when `target` closes: the previous
 * sibling's nearest pane, or the next sibling's when `target` was first.
 * Returns undefined when `target` is the root or absent. O(n).
 */
export function paneReceivingSpace(root: LayoutNode, target: PaneId): PaneId | undefined {
  if (root.kind === "pane") return undefined;
  const index = root.children.findIndex((child) => child.kind === "pane" && child.id === target);
  if (index >= 0) {
    const receiver = index > 0 ? root.children[index - 1] : root.children[index + 1];
    if (!receiver) return undefined;
    const ids = paneIds(receiver);
    // The side of the receiver that touches the closed pane.
    return index > 0 ? ids.at(-1) : ids[0];
  }
  for (const child of root.children) {
    const found = paneReceivingSpace(child, target);
    if (found) return found;
  }
  return undefined;
}

/** Each pane's rectangle in unit coordinates: the root covers [0,1] x [0,1]. O(n). */
export function paneRects(root: LayoutNode): Map<PaneId, Rect> {
  const rects = new Map<PaneId, Rect>();
  const visit = (node: LayoutNode, rect: Rect) => {
    if (node.kind === "pane") {
      rects.set(node.id, rect);
      return;
    }
    let offset = 0;
    node.children.forEach((child, index) => {
      const share = (node.sizes[index] ?? 0) / TOTAL;
      const childRect: Rect =
        node.direction === "row"
          ? {
              x: rect.x + offset * rect.width,
              y: rect.y,
              width: share * rect.width,
              height: rect.height,
            }
          : {
              x: rect.x,
              y: rect.y + offset * rect.height,
              width: rect.width,
              height: share * rect.height,
            };
      offset += share;
      visit(child, childRect);
    });
  };
  visit(root, { x: 0, y: 0, width: 1, height: 1 });
  return rects;
}

/**
 * The pane next to `from` on `side`, as the user sees it on screen.
 *
 * Candidates lie entirely beyond `from`'s edge on that side. Among them the
 * closest edge wins; ties go to the largest overlap along the other axis,
 * then to the candidate nearest `from`'s centre. O(n).
 */
export function neighbor(root: LayoutNode, from: PaneId, side: Side): PaneId | undefined {
  const rects = paneRects(root);
  const origin = rects.get(from);
  if (!origin) {
    return undefined;
  }
  const eps = 1e-9;
  let best: { id: PaneId; gap: number; overlap: number; offset: number } | undefined;
  for (const [id, rect] of rects) {
    if (id === from) continue;
    const gap = edgeGap(origin, rect, side);
    if (gap < -eps) continue;
    const overlap = perpendicularOverlap(origin, rect, side);
    if (overlap <= eps) continue;
    const offset = centreOffset(origin, rect, side);
    const better =
      !best ||
      gap < best.gap - eps ||
      (Math.abs(gap - best.gap) <= eps &&
        (overlap > best.overlap + eps ||
          (Math.abs(overlap - best.overlap) <= eps && offset < best.offset)));
    if (better) {
      best = { id, gap, overlap, offset };
    }
  }
  return best?.id;
}

/** Throws when an invariant is broken. For tests and debug assertions. */
export function assertValid(root: LayoutNode): void {
  const seen = new Set<string>();
  const visit = (node: LayoutNode, parentDirection: Direction | undefined) => {
    if (seen.has(node.id)) throw new Error(`duplicate node id ${node.id}`);
    seen.add(node.id);
    if (node.kind === "pane") return;
    if (node.children.length < 2) throw new Error(`split ${node.id} has fewer than two children`);
    if (node.sizes.length !== node.children.length)
      throw new Error(`split ${node.id} sizes mismatch`);
    if (node.sizes.some((s) => !(s > 0)))
      throw new Error(`split ${node.id} has a non-positive size`);
    const sum = node.sizes.reduce((a, b) => a + b, 0);
    if (Math.abs(sum - TOTAL) > 1e-6) throw new Error(`split ${node.id} sizes sum to ${sum}`);
    if (node.direction === parentDirection)
      throw new Error(`split ${node.id} repeats its parent's direction`);
    for (const child of node.children) visit(child, node.direction);
  };
  visit(root, undefined);
}

type SplitNode = Extract<LayoutNode, { kind: "split" }>;

/** What to put in place of the target pane: nothing (remove) or new nodes. */
type Replace = (leaf: LayoutNode, parent: SplitNode | undefined) => LayoutNode[];

/**
 * Replaces pane `target` with what `replace` returns, then restores the
 * invariants on the way back up. Returns `undefined` when `target` is absent,
 * so callers keep the original tree, and `null` when nothing is left.
 */
function mapPane(
  root: LayoutNode,
  target: PaneId,
  replace: Replace,
): LayoutNode | null | undefined {
  if (root.kind === "pane") {
    if (root.id !== target) return undefined;
    const [first, ...rest] = replace(root, undefined);
    if (!first) return null;
    if (rest.length > 0) throw new Error("a root pane can only be replaced by one node");
    return first;
  }
  const result = rebuild(root, target, replace);
  return result === NOT_FOUND ? undefined : result;
}

const NOT_FOUND = Symbol("not found");

function rebuild(
  node: SplitNode,
  target: PaneId,
  replace: Replace,
): LayoutNode | null | typeof NOT_FOUND {
  const children: LayoutNode[] = [];
  const sizes: number[] = [];
  // Space of a removed child: `receiver` is its previous sibling's index, or
  // -1 to hand it to whichever child ends up first.
  let freed: { receiver: number; size: number } | undefined;
  let found = false;

  node.children.forEach((child, index) => {
    const size = node.sizes[index] ?? 0;
    if (!found) {
      const replacement =
        child.kind === "pane"
          ? child.id === target
            ? replace(child, node)
            : undefined
          : wrap(rebuild(child, target, replace));
      if (replacement) {
        found = true;
        if (replacement.length === 0) {
          freed = { receiver: children.length - 1, size };
        } else {
          for (const created of replacement) {
            children.push(created);
            sizes.push(size / replacement.length);
          }
        }
        return;
      }
    }
    children.push(child);
    sizes.push(size);
  });

  if (!found) return NOT_FOUND;
  if (freed && sizes.length > 0) {
    const index = Math.max(freed.receiver, 0);
    sizes[index] = (sizes[index] ?? 0) + freed.size;
  }
  return canonical({ ...node, children, sizes });
}

/** Adapts a nested rebuild result to the replacement list of its parent. */
function wrap(result: LayoutNode | null | typeof NOT_FOUND): LayoutNode[] | undefined {
  if (result === NOT_FOUND) return undefined;
  return result === null ? [] : [result];
}

/** Collapses single-child splits and flattens same-direction children. */
function canonical(node: SplitNode): LayoutNode | null {
  if (node.children.length === 0) return null;
  if (node.children.length === 1) return node.children[0] ?? null;
  const children: LayoutNode[] = [];
  const sizes: number[] = [];
  node.children.forEach((child, index) => {
    const size = node.sizes[index] ?? 0;
    if (child.kind === "split" && child.direction === node.direction) {
      child.children.forEach((grandchild, j) => {
        children.push(grandchild);
        sizes.push((size * (child.sizes[j] ?? 0)) / TOTAL);
      });
    } else {
      children.push(child);
      sizes.push(size);
    }
  });
  return { ...node, children, sizes: normalize(sizes) };
}

/**
 * Scales sizes to sum to 100. When the plain sum would overflow (sizes near
 * `Number.MAX_VALUE`), sizes are first divided by the largest one; the common
 * path keeps the exact formula so ordinary sizes do not lose precision.
 */
function normalize(sizes: readonly number[]): number[] {
  const sum = sizes.reduce((a, b) => a + b, 0);
  if (Number.isFinite(sum) && sum > 0) {
    return sizes.map((s) => (s * TOTAL) / sum);
  }
  const largest = Math.max(...sizes);
  if (!(largest > 0) || !Number.isFinite(largest)) {
    return sizes.map(() => TOTAL / sizes.length);
  }
  const scaled = sizes.map((s) => s / largest);
  const scaledSum = scaled.reduce((a, b) => a + b, 0);
  return scaled.map((s) => (s * TOTAL) / scaledSum);
}

function edgeGap(from: Rect, to: Rect, side: Side): number {
  const gaps: Record<Side, number> = {
    left: from.x - (to.x + to.width),
    right: to.x - (from.x + from.width),
    up: from.y - (to.y + to.height),
    down: to.y - (from.y + from.height),
  };
  return gaps[side];
}

function perpendicularOverlap(from: Rect, to: Rect, side: Side): number {
  const horizontal = side === "left" || side === "right";
  const [a0, a1, b0, b1] = horizontal
    ? [from.y, from.y + from.height, to.y, to.y + to.height]
    : [from.x, from.x + from.width, to.x, to.x + to.width];
  return Math.min(a1, b1) - Math.max(a0, b0);
}

function centreOffset(from: Rect, to: Rect, side: Side): number {
  const horizontal = side === "left" || side === "right";
  return horizontal
    ? Math.abs(from.y + from.height / 2 - (to.y + to.height / 2))
    : Math.abs(from.x + from.width / 2 - (to.x + to.width / 2));
}
