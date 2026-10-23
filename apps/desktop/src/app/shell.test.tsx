// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "vitest-browser-react";

import { paneIds, paneRects } from "@/features/workspace/layout.ts";
import { PaneTree } from "@/features/workspace/PaneTree.tsx";
import { fakeSessions } from "@/ipc/fake.ts";
import { fakeHosts } from "@/ipc/fake-hosts.ts";
import { fakeSnippets } from "@/ipc/fake-snippets.ts";
import { fakeVault } from "@/ipc/fake-vault.ts";

import { AppShell } from "./shell.ts";

const shells: AppShell[] = [];
afterEach(() => {
  for (const made of shells.splice(0)) made.dispose();
});

function shell(hosts = fakeHosts()): AppShell {
  const made = new AppShell({
    sessions: fakeSessions,
    hosts,
    vault: fakeVault(),
    snippets: fakeSnippets(() => []),
    userAgent: "test",
    storage: localStorage,
    root: document.documentElement,
    clipboard: undefined,
  });
  shells.push(made);
  return made;
}

/** Shows the active tab, so its panes open their sessions. */
async function show(app: AppShell) {
  const tab = app.workspace.activeTab;
  if (!tab) throw new Error("no tab");
  return render(
    <div style={{ width: 1200, height: 800 }}>
      <PaneTree workspace={app.workspace} tab={tab} broadcast={app.broadcast} />
    </div>,
  );
}

describe("AppShell broadcast", () => {
  it("types into panes that run, in the same tab, and not into one still opening", async () => {
    const app = shell();
    app.workspace.newTab();
    app.run("pane.splitRight");
    const tab = app.workspace.activeTab!;
    const [left, right] = paneIds(tab.root);
    await show(app);
    const one = app.workspace.terminal(left!)!;
    const two = app.workspace.terminal(right!)!;
    await vi.waitFor(() => expect(two.getStatus().kind).toBe("running"));
    await vi.waitFor(() => expect(one.getStatus().kind).toBe("running"));
    const typed = vi.spyOn(two, "typeShared");
    app.run("broadcast.tab");
    expect(app.broadcast.getSnapshot().size).toBe(2);

    one.terminal.input("ls");
    expect(typed).toHaveBeenCalledWith("ls");

    // A pane of another tab, though added, gets nothing from this one.
    app.workspace.newTab();
    const other = app.workspace.activeTab!.focused;
    app.broadcast.toggle(other);
    const elsewhere = vi.spyOn(app.workspace.terminal(other)!, "type");
    one.terminal.input("x");
    expect(elsewhere).not.toHaveBeenCalled();

    // A closed pane leaves at once.
    app.workspace.closePane(right);
    expect(app.broadcast.has(right!)).toBe(false);
  });

  it("keeps the keys a click or wheel makes in their pane", async () => {
    const app = shell();
    app.workspace.newTab();
    app.run("pane.splitRight");
    const [left, right] = paneIds(app.workspace.activeTab!.root);
    await show(app);
    const one = app.workspace.terminal(left!)!;
    const two = app.workspace.terminal(right!)!;
    await vi.waitFor(() => expect(two.getStatus().kind).toBe("running"));
    app.run("broadcast.tab");
    const typed = vi.spyOn(two, "typeShared");
    // xterm.js makes these keys while it handles the event, as here.
    const inner = one.terminal.element?.querySelector(".xterm-screen");
    inner?.addEventListener("wheel", () => one.terminal.input("\u001b[A"), { once: true });
    inner?.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, bubbles: true }));
    expect(typed).not.toHaveBeenCalled();
    // Text an IME commits on its own timer, after the event, is the user's.
    one.terminal.input("tiếng");
    expect(typed).toHaveBeenCalledWith("tiếng");
  });

  it("keeps a whole secret from panes not asking for one, though the prompt echoes *", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      const app = shell();
      app.workspace.newTab();
      app.run("pane.splitRight");
      const [left, right] = paneIds(app.workspace.activeTab!.root);
      await show(app);
      const one = app.workspace.terminal(left!)!;
      const two = app.workspace.terminal(right!)!;
      await vi.waitFor(() => expect(two.getStatus().kind).toBe("running"));
      app.run("broadcast.tab");
      const typed = vi.spyOn(two, "typeShared").mockImplementation(() => {});
      // That host has sudo cached: it never asks.
      vi.spyOn(two, "atSecretPrompt", "get").mockReturnValue(false);
      // Typed ahead of the prompt: held, then judged once it shows.
      one.terminal.input("sudo ls\r");
      one.terminal.input("h");
      expect(typed.mock.calls.map(([d]) => d)).toEqual(["sudo ls\r"]);
      await new Promise<void>((resolve) => {
        one.terminal.write("\r\n[sudo] password for me: ", resolve);
      });
      vi.advanceTimersByTime(600);
      for (const key of ["u", "n", "\r"]) {
        one.terminal.input(key);
        // oxlint-disable-next-line no-await-in-loop -- each echo lands before the next key
        await new Promise<void>((resolve) => {
          one.terminal.write("*", resolve);
        });
      }
      expect(typed.mock.calls.map(([d]) => d)).toEqual(["sudo ls\r"]);
      // After Enter the secret is over: the shell shows its prompt, and once
      // the hold passes, keys go again.
      one.terminal.write("\r\n$ ");
      await vi.advanceTimersByTimeAsync(600);
      expect(one.atSecretPrompt).toBe(false);
      one.terminal.input("x");
      expect(typed.mock.calls.map(([d]) => d)).toEqual(["sudo ls\r", "x"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("opens many hosts as an even grid that broadcasts together", async () => {
    const hosts = fakeHosts();
    for (let i = 0; i < 5; i += 1) {
      // oxlint-disable-next-line no-await-in-loop -- each save returns the list so far
      await hosts.saveHost({ label: `web${i}`, address: "prompt.test" });
    }
    const made = shell(hosts);
    await made.hosts.load();
    await made.snippets!.save({ id: null, name: "up", folder: null, body: "uptime" });
    const ids = made.hosts.getSnapshot().view.hosts.map((h) => h.id);
    await made.runSnippet("snippet-1", {}, ids);
    const tab = made.workspace.activeTab!;
    const panes = paneIds(tab.root);
    expect(panes).toHaveLength(5);
    expect(made.broadcast.getSnapshot().size).toBe(5);
    // Three columns, two rows: no pane under a sixth of the tab.
    const rects = [...paneRects(tab.root).values()];
    for (const rect of rects) expect(rect.width * rect.height).toBeGreaterThan(1 / 6 - 1e-9);
  });
});

describe("AppShell workspaces", () => {
  it("keeps a proxied connection as a local shell, and opens a deleted host as one", async () => {
    localStorage.removeItem("tauminus.workspaces");
    const hosts = fakeHosts();
    await hosts.saveHost({ label: "db", address: "prompt.test" });
    const app = shell(hosts);
    await app.hosts.load();
    const db = app.hosts.getSnapshot().view.hosts[0]!;
    app.workspace.newTab({ kind: "host", hostId: db.id, label: db.label, target: db.target! });
    app.workspace.split("row", {
      kind: "ssh",
      target: {
        destination: { user: "me", host: "inner", port: 22 },
        route: { kind: "proxyCommand", command: "nc %h %p" },
      },
    });
    expect(app.saveWorkspace("ops")).toMatch(/me@inner.*local shell/u);
    const [saved] = app.workspaces.getSnapshot();
    expect(JSON.stringify(saved)).not.toContain("nc %h");

    await hosts.deleteHost(db.id);
    await app.hosts.load();
    await app.openWorkspace(saved!.id);
    expect(app.hosts.getSnapshot().notice).toMatch(/ops: a deleted host opened as a local shell/u);
    expect(app.workspace.focusedSpec()).toEqual({ kind: "local" });
  });

  it("waits for the saved hosts before it opens one, and never guesses", async () => {
    localStorage.removeItem("tauminus.workspaces");
    const hosts = fakeHosts();
    await hosts.saveHost({ label: "db", address: "prompt.test" });
    const app = shell(hosts);
    await app.hosts.load();
    const db = app.hosts.getSnapshot().view.hosts[0]!;
    app.workspace.newTab({ kind: "host", hostId: db.id, label: db.label, target: db.target! });
    app.saveWorkspace("ops");
    const [saved] = app.workspaces.getSnapshot();

    // A new window, its host list not read yet.
    const fresh = shell(hosts);
    expect(fresh.hosts.getSnapshot().status).toBe("loading");
    await fresh.openWorkspace(saved!.id);
    expect(fresh.workspace.focusedSpec()).toMatchObject({ kind: "host", hostId: db.id });

    // One whose list failed opens nothing.
    const broken = shell({ ...hosts, list: () => Promise.reject(new Error("disk")) });
    await broken.openWorkspace(saved!.id);
    expect(broken.workspace.getSnapshot().tabs).toHaveLength(0);
    expect(broken.hosts.getSnapshot().notice).toMatch(/not opened/u);
  });
});
