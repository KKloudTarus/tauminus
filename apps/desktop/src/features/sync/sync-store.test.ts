// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { fakeSync } from "@/ipc/fake-sync.ts";
import { sharedFakeVault } from "@/ipc/fake-vault.ts";

import { SyncStore } from "./sync-store.ts";

async function unlocked(): Promise<void> {
  const vault = sharedFakeVault();
  if (!(await vault.status()).exists) await vault.create("vault password");
  else await vault.unlock("vault password");
}

describe("SyncStore", () => {
  it("shows a remote without its token and keeps no passphrase", async () => {
    await unlocked();
    const store = new SyncStore(fakeSync());
    await store.refresh();
    expect(store.getSnapshot().status?.setup).toEqual({ kind: "off" });
    await expect(
      store.create({ kind: "git", address: "https://me:tok@example.com/s.git" }, "short"),
    ).rejects.toThrow("12 characters");
    await store.create(
      { kind: "git", address: "https://me:tok@example.com/s.git" },
      "a long sync passphrase",
    );
    expect(store.getSnapshot().status?.setup).toEqual({
      kind: "on",
      through: "git",
      shown: "https://example.com/s.git",
    });
    expect(JSON.stringify(store.getSnapshot())).not.toContain("tok");
    expect(JSON.stringify(store.getSnapshot())).not.toContain("passphrase");
    await store.leave();
    expect(store.getSnapshot().status?.setup).toEqual({ kind: "off" });
  });

  it("lists what another device changed until it is accepted as shown", async () => {
    await unlocked();
    let synced = 0;
    const store = new SyncStore(fakeSync(), () => (synced += 1));
    await expect(
      store.join({ kind: "folder", address: "/srv/sync" }, "not their passphrase"),
    ).rejects.toThrow("does not open");
    await store.join({ kind: "folder", address: "/srv/sync" }, "correct horse battery");
    await store.now();
    expect(synced).toBe(1);
    expect(store.getSnapshot().status?.last?.clashes).toHaveLength(1);
    const [identity, review] = store.getSnapshot().reviews;
    expect(identity?.host).toBe("identity:deploy1");
    expect(review?.label).toBe("staging");
    if (!review || !identity) return;
    expect(store.getSnapshot().noted).toBe(2);
    expect(await store.accept([{ ...review, fingerprint: "00".repeat(32) }])).toEqual(["staging"]);
    expect(store.getSnapshot().reviews).toHaveLength(2);
    expect(await store.accept([identity, review])).toEqual([]);
    expect(store.getSnapshot()).toMatchObject({ reviews: [], noted: 0 });
  });

  it("says the vault is locked", async () => {
    await unlocked();
    await sharedFakeVault().lock();
    const store = new SyncStore(fakeSync());
    await store.refresh();
    expect(store.getSnapshot().status?.setup).toEqual({ kind: "locked" });
    await expect(store.now()).rejects.toThrow("locked");
  });
});
