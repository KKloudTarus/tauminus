// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { MAX_WORKSPACES, type SavedSpec, SavedWorkspaces, directConnections } from "./saved.ts";
import type { Template } from "./layout.ts";

function memory(initial?: string): Storage {
  const items = new Map<string, string>(initial ? [["tauminus.workspaces", initial]] : []);
  return {
    get length() {
      return items.size;
    },
    clear: () => items.clear(),
    getItem: (key) => items.get(key) ?? null,
    key: (index) => [...items.keys()][index] ?? null,
    removeItem: (key) => items.delete(key),
    setItem: (key, value) => items.set(key, value),
  };
}

const local: Template<SavedSpec> = { kind: "pane", spec: { kind: "local" } };

/** Storage holding one workspace whose local pane names `shell`. */
const holding = (shell: unknown) =>
  JSON.stringify([
    { id: "x", name: "x", tabs: [{ kind: "pane", spec: { kind: "local", shell } }] },
  ]);
const grid: Template<SavedSpec> = {
  kind: "split",
  direction: "row",
  sizes: [40, 60],
  children: [
    { kind: "pane", spec: { kind: "host", hostId: "db" } },
    {
      kind: "pane",
      spec: { kind: "ssh", destination: { user: "me", host: "web", port: 22 }, jumps: [] },
    },
  ],
};

describe("SavedWorkspaces", () => {
  it("keeps what it saves across a reload, and replaces by name", () => {
    const storage = memory();
    const saved = new SavedWorkspaces(storage);
    expect(saved.save("dev", [grid, local])).toBe(true);
    expect(saved.save("  ", [local])).toBe(false);
    expect(saved.save("x", [])).toBe(false);
    const again = new SavedWorkspaces(storage);
    expect(again.getSnapshot().map((w) => [w.name, w.tabs])).toEqual([["dev", [grid, local]]]);
    expect(again.save("dev", [local])).toBe(true);
    expect(again.getSnapshot()).toHaveLength(1);
    expect(again.getSnapshot()[0]?.tabs).toEqual([local]);
    again.remove(again.getSnapshot()[0]!.id);
    expect(new SavedWorkspaces(storage).getSnapshot()).toEqual([]);
  });

  it("keeps a serial pane's port and settings, and drops broken ones", () => {
    const storage = memory();
    const serial: Template<SavedSpec> = {
      kind: "pane",
      spec: {
        kind: "serial",
        port: {
          path: "/dev/ttyUSB0",
          baud: 115_200,
          dataBits: 8,
          parity: "none",
          stopBits: 1,
          flow: "hardware",
          encoding: "KOI8-R",
        },
      },
    };
    new SavedWorkspaces(storage).save("bench", [serial]);
    const [bench] = new SavedWorkspaces(storage).getSnapshot();
    expect(bench?.tabs).toEqual([serial]);
    // Shown before it opens, as a connection is.
    expect(directConnections(bench!)).toEqual(["serial /dev/ttyUSB0 115200 8N1"]);
    const broken = JSON.stringify([
      {
        id: "x",
        name: "x",
        tabs: [{ kind: "pane", spec: { kind: "serial", port: { path: "", baud: 9600 } } }],
      },
    ]);
    expect(new SavedWorkspaces(memory(broken)).getSnapshot()).toEqual([]);
  });

  it("keeps a local pane's shell, and opens the default for one it cannot read", () => {
    const storage = memory();
    const pwsh: Template<SavedSpec> = {
      kind: "pane",
      spec: { kind: "local", shell: { id: "pwsh", name: "PowerShell 7" } },
    };
    new SavedWorkspaces(storage).save("win", [pwsh, local]);
    expect(new SavedWorkspaces(storage).getSnapshot()[0]?.tabs).toEqual([pwsh, local]);
    for (const shell of [
      "pwsh",
      { id: "pwsh" },
      { id: "", name: "PowerShell 7" },
      { id: 7, name: "PowerShell 7" },
      { id: "x".repeat(200), name: "PowerShell 7" },
    ]) {
      expect(new SavedWorkspaces(memory(holding(shell))).getSnapshot()[0]?.tabs).toEqual([local]);
    }
  });

  it("stops at its limit", () => {
    const saved = new SavedWorkspaces(memory());
    for (let i = 0; i < MAX_WORKSPACES; i += 1) expect(saved.save(`w${i}`, [local])).toBe(true);
    expect(saved.save("one more", [local])).toBe(false);
    expect(saved.save("w0", [grid])).toBe(true);
  });

  it("drops what storage holds that it would not have written", () => {
    const deep = (n: number): unknown =>
      n === 0
        ? { kind: "pane", spec: { kind: "local" } }
        : { kind: "split", direction: "row", sizes: [50, 50], children: [deep(n - 1), deep(0)] };
    const bad = [
      "not json",
      JSON.stringify({ not: "a list" }),
      JSON.stringify([
        { id: "a", name: "", tabs: [local] },
        { id: "b", name: "proxy", tabs: [{ kind: "pane", spec: { kind: "ssh", route: "x" } }] },
        {
          id: "c",
          name: "port",
          tabs: [
            {
              kind: "pane",
              spec: { kind: "ssh", destination: { user: "", host: "h", port: 70000 }, jumps: [] },
            },
          ],
        },
        { id: "d", name: "deep", tabs: [deep(40)] },
        { id: "e", name: "huge", tabs: Array.from({ length: 65 }, () => local) },
        { id: "f", name: "fine", tabs: [local] },
      ]),
    ];
    expect(new SavedWorkspaces(memory(bad[0])).getSnapshot()).toEqual([]);
    expect(new SavedWorkspaces(memory(bad[1])).getSnapshot()).toEqual([]);
    expect(new SavedWorkspaces(memory(bad[2])).getSnapshot().map((w) => w.name)).toEqual(["fine"]);
  });
});
