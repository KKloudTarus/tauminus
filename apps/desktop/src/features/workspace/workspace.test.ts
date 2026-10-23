// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { assertValid, paneIds } from "./layout.ts";
import { MAX_PANES, MAX_PANES_PER_TAB, type PaneTerminal, Workspace } from "./workspace.ts";

class FakeTerminal implements PaneTerminal {
  status: { kind: string; code?: number | null } = { kind: "running" };
  disposed = 0;
  focused = 0;
  #listeners = new Set<() => void>();

  getStatus() {
    return this.status;
  }

  subscribe(listener: () => void) {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  focus() {
    this.focused += 1;
  }

  dispose() {
    this.disposed += 1;
  }

  exit(code: number | null) {
    this.status = { kind: "exited", code };
    for (const listener of this.#listeners) listener();
  }
}

function setup() {
  const terminals = new Map<string, FakeTerminal>();
  const workspace = new Workspace((paneId) => {
    const terminal = new FakeTerminal();
    terminals.set(paneId, terminal);
    return terminal;
  });
  return { workspace, terminals };
}

const tabIds = (w: Workspace) => w.getSnapshot().tabs.map((t) => t.id);

describe("Workspace tabs", () => {
  it("opens tabs and activates the newest", () => {
    const { workspace } = setup();
    workspace.newTab();
    workspace.newTab();
    const { tabs, activeTabId } = workspace.getSnapshot();
    expect(tabs).toHaveLength(2);
    expect(activeTabId).toBe(tabs[1]?.id);
  });

  it("closing the active tab activates the next one, or the previous at the end", () => {
    const { workspace } = setup();
    workspace.newTab();
    workspace.newTab();
    workspace.newTab();
    const [a, b, c] = tabIds(workspace);
    workspace.activateTab(b!);
    workspace.closeTab(b!);
    expect(workspace.getSnapshot().activeTabId).toBe(c);
    workspace.closeTab(c!);
    expect(workspace.getSnapshot().activeTabId).toBe(a);
  });

  it("closing a tab disposes each of its terminals once", () => {
    const { workspace, terminals } = setup();
    workspace.newTab();
    workspace.split("row");
    const tab = workspace.activeTab!;
    workspace.closeTab(tab.id);
    for (const id of paneIds(tab.root)) {
      expect(terminals.get(id)?.disposed).toBe(1);
    }
    expect(workspace.paneCount).toBe(0);
  });

  it("cycles through tabs in both directions with wrap-around", () => {
    const { workspace } = setup();
    workspace.newTab();
    workspace.newTab();
    const [a, b] = tabIds(workspace);
    workspace.cycleTab(1);
    expect(workspace.getSnapshot().activeTabId).toBe(a);
    workspace.cycleTab(-1);
    expect(workspace.getSnapshot().activeTabId).toBe(b);
  });

  it("moves tabs and clamps the target position", () => {
    const { workspace } = setup();
    workspace.newTab();
    workspace.newTab();
    workspace.newTab();
    const [a, b, c] = tabIds(workspace);
    workspace.moveTab(a!, 2);
    expect(tabIds(workspace)).toEqual([b, c, a]);
    workspace.moveTab(a!, -5);
    expect(tabIds(workspace)).toEqual([a, b, c]);
    workspace.activateTab(c!);
    workspace.moveActiveTab(-1);
    expect(tabIds(workspace)).toEqual([a, c, b]);
    workspace.moveTab(a!, Number.NaN);
    expect(tabIds(workspace)).toEqual([a, c, b]);
  });

  it("publishes a new snapshot only when something changes", () => {
    const { workspace } = setup();
    workspace.newTab();
    const before = workspace.getSnapshot();
    workspace.moveTab("missing", 0);
    workspace.activateTab("missing");
    expect(workspace.getSnapshot()).toBe(before);
  });
});

describe("Workspace specs", () => {
  it("opens each pane with its spec and splits with the focused pane's", () => {
    const opened: [string, string | undefined][] = [];
    const workspace = new Workspace<FakeTerminal, string>((paneId, spec) => {
      opened.push([paneId, spec]);
      return new FakeTerminal();
    });
    workspace.newTab();
    workspace.newTab("ssh:db");
    workspace.split("row");
    workspace.split("column", "ssh:web");
    expect(opened.map(([, spec]) => spec)).toEqual([undefined, "ssh:db", "ssh:db", "ssh:web"]);
    expect(workspace.focusedSpec()).toBe("ssh:web");
  });
});

describe("Workspace panes", () => {
  it("splits the focused pane and focuses the new one", () => {
    const { workspace } = setup();
    workspace.newTab();
    const first = workspace.activeTab!.focused;
    expect(workspace.split("row")).toBe(true);
    const tab = workspace.activeTab!;
    expect(paneIds(tab.root)).toHaveLength(2);
    expect(tab.focused).not.toBe(first);
  });

  it(`refuses a split beyond ${MAX_PANES_PER_TAB} panes in a tab`, () => {
    const { workspace } = setup();
    workspace.newTab();
    for (let i = 1; i < MAX_PANES_PER_TAB; i += 1) {
      expect(workspace.split(i % 2 ? "row" : "column")).toBe(true);
    }
    expect(workspace.split("row")).toBe(false);
    expect(paneIds(workspace.activeTab!.root)).toHaveLength(MAX_PANES_PER_TAB);
  });

  it(`refuses new panes beyond ${MAX_PANES} in total`, () => {
    const { workspace } = setup();
    for (let i = 0; i < MAX_PANES; i += 1) {
      expect(workspace.newTab()).toBe(true);
    }
    expect(workspace.newTab()).toBe(false);
    expect(workspace.split("row")).toBe(false);
  });

  it("closing the last pane closes its tab", () => {
    const { workspace } = setup();
    workspace.newTab();
    workspace.closePane();
    expect(workspace.getSnapshot().tabs).toHaveLength(0);
  });

  it("closing the focused pane moves focus to its neighbour", () => {
    const { workspace, terminals } = setup();
    workspace.newTab();
    const left = workspace.activeTab!.focused;
    workspace.split("row");
    const right = workspace.activeTab!.focused;
    workspace.closePane();
    expect(workspace.activeTab!.focused).toBe(left);
    expect(terminals.get(right)?.disposed).toBe(1);
    expect(terminals.get(left)?.disposed).toBe(0);
  });

  it("gives focus to the pane that grows when the focused pane closes", () => {
    // row[A, column[B, C]]: closing B grows C, so C gets focus, not A.
    const { workspace } = setup();
    workspace.newTab();
    workspace.split("row");
    const b = workspace.activeTab!.focused;
    workspace.split("column");
    const c = workspace.activeTab!.focused;
    workspace.focusPane(b);
    workspace.closePane();
    expect(workspace.activeTab!.focused).toBe(c);
  });

  it("moves focus spatially and focuses the terminal", () => {
    const { workspace, terminals } = setup();
    workspace.newTab();
    const left = workspace.activeTab!.focused;
    workspace.split("row");
    const right = workspace.activeTab!.focused;
    workspace.focusNeighbor("left");
    expect(workspace.activeTab!.focused).toBe(left);
    expect(terminals.get(left)?.focused).toBe(1);
    workspace.focusNeighbor("left");
    expect(workspace.activeTab!.focused).toBe(left);
    workspace.focusNeighbor("right");
    expect(workspace.activeTab!.focused).toBe(right);
  });

  it("focusing a pane in another tab activates that tab", () => {
    const { workspace } = setup();
    workspace.newTab();
    const firstPane = workspace.activeTab!.focused;
    const firstTab = workspace.activeTab!.id;
    workspace.newTab();
    workspace.focusPane(firstPane);
    expect(workspace.getSnapshot().activeTabId).toBe(firstTab);
  });

  it("closes a pane whose shell exits with code 0", async () => {
    const { workspace, terminals } = setup();
    workspace.newTab();
    workspace.split("row");
    const pane = workspace.activeTab!.focused;
    terminals.get(pane)?.exit(0);
    await Promise.resolve();
    expect(paneIds(workspace.activeTab!.root)).not.toContain(pane);
    expect(terminals.get(pane)?.disposed).toBe(1);
  });

  it.each([1, null])(
    "keeps a pane whose shell exits with %s, so the message stays readable",
    async (code) => {
      const { workspace, terminals } = setup();
      workspace.newTab();
      const pane = workspace.activeTab!.focused;
      terminals.get(pane)?.exit(code);
      await Promise.resolve();
      expect(paneIds(workspace.activeTab!.root)).toContain(pane);
    },
  );

  it("dispose releases every terminal once and ignores later calls", () => {
    const { workspace, terminals } = setup();
    workspace.newTab();
    workspace.split("column");
    workspace.newTab();
    workspace.dispose();
    for (const terminal of terminals.values()) {
      expect(terminal.disposed).toBe(1);
    }
    expect(workspace.newTab()).toBe(false);
  });
});

const leaf = (spec: string) => ({ kind: "pane" as const, spec });

describe("Workspace templates", () => {
  it("opens a saved layout again with its specs and shares", () => {
    const opened: (string | undefined)[] = [];
    const workspace = new Workspace<FakeTerminal, string>((_id, spec) => {
      opened.push(spec);
      return new FakeTerminal();
    });
    workspace.newTab("a");
    workspace.split("row", "b");
    workspace.split("column", "c");
    const [template] = workspace.templates();
    expect(workspace.openTemplate(template!)).toBe(true);
    expect(opened).toEqual(["a", "b", "c", "a", "b", "c"]);
    const tab = workspace.activeTab!;
    assertValid(tab.root);
    expect(workspace.templates()[1]).toEqual(template);
  });

  it("repairs a template read from storage, and refuses one past the limits", () => {
    const workspace = new Workspace<FakeTerminal, string>(() => new FakeTerminal());
    // One child, bad sizes, and a child split in its parent's direction.
    expect(
      workspace.openTemplate({
        kind: "split",
        direction: "row",
        sizes: [-1, Number.NaN],
        children: [
          {
            kind: "split",
            direction: "row",
            sizes: [50, 50],
            children: [leaf("a"), leaf("b")],
          },
          { kind: "split", direction: "column", sizes: [100], children: [leaf("c")] },
        ],
      }),
    ).toBe(true);
    const root = workspace.activeTab!.root;
    assertValid(root);
    expect(paneIds(root)).toHaveLength(3);
    const many = {
      kind: "split" as const,
      direction: "row" as const,
      sizes: [],
      children: Array.from({ length: MAX_PANES_PER_TAB + 1 }, () => leaf("x")),
    };
    expect(workspace.openTemplate(many)).toBe(false);
    expect(workspace.openTemplate({ ...many, children: [] })).toBe(false);
    // Shares far apart are shared equally; the layout stays valid.
    for (const sizes of [
      [1e308, 1e308],
      [5e-324, 1],
      [1e-300, 1],
    ]) {
      expect(
        workspace.openTemplate({
          kind: "split",
          direction: "column",
          sizes,
          children: [leaf("a"), leaf("b")],
        }),
      ).toBe(true);
      assertValid(workspace.activeTab!.root);
      expect(workspace.activeTab!.root).toMatchObject({ sizes: [50, 50] });
    }
  });
});
