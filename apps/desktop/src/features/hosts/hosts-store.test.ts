// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { fakeHosts } from "@/ipc/fake-hosts.ts";
import type { HostsClient } from "@/ipc/index.ts";

import { HostsStore } from "./hosts-store.ts";

describe("HostsStore", () => {
  it("loads the list and the import offer", async () => {
    const store = new HostsStore(fakeHosts());
    expect(store.getSnapshot().status).toBe("loading");
    await store.load();
    expect(store.getSnapshot()).toMatchObject({ status: "ready", configAliases: 3 });
  });

  it("shows only what the backend returned after a change", async () => {
    const store = new HostsStore(fakeHosts());
    await store.load();
    await store.saveGroup({ name: "prod" });
    const group = store.getSnapshot().view.groups[0]!;
    await store.saveHost({ label: "db", address: "db.internal", group: group.id });
    const host = store.getSnapshot().view.hosts[0]!;
    expect(store.groupName(host)).toBe("prod");
    await store.deleteGroup(group.id);
    expect(store.groupName(store.getSnapshot().view.hosts[0]!)).toBe("");
  });

  it("keeps the old list when a save fails", async () => {
    const client: HostsClient = {
      ...fakeHosts(),
      saveHost: async () => {
        throw new Error("disk full");
      },
    };
    const store = new HostsStore(client);
    await store.load();
    const before = store.getSnapshot();
    await expect(store.saveHost({ label: "x", address: "x" })).rejects.toThrow("disk full");
    expect(store.getSnapshot()).toBe(before);
  });

  it("reports a list that cannot load", async () => {
    const client: HostsClient = {
      ...fakeHosts(),
      list: async () => {
        throw new Error("hosts.json is unreadable");
      },
    };
    const store = new HostsStore(client);
    await store.load();
    expect(store.getSnapshot()).toMatchObject({
      status: "failed",
      error: "hosts.json is unreadable",
    });
  });

  it("summarizes an import, including what it skipped", async () => {
    const store = new HostsStore(fakeHosts());
    await store.load();
    await store.importConfig();
    expect(store.getSnapshot().notice).toBe("Imported 3 hosts from ~/.ssh/config");
    await store.importConfig();
    expect(store.getSnapshot().notice).toMatch(
      /^Imported 0 hosts from ~\/\.ssh\/config; skipped 3: prompt \(already saved\)/,
    );
    store.dismissNotice();
    expect(store.getSnapshot().notice).toBeUndefined();
  });
});
