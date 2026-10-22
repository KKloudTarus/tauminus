// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import { fakeVault } from "@/ipc/fake-vault.ts";

import { TOUCH_EVERY_MS, VaultStore } from "./vault-store.ts";

class MemoryStorage implements Storage {
  readonly #items = new Map<string, string>();
  get length(): number {
    return this.#items.size;
  }
  clear(): void {
    this.#items.clear();
  }
  getItem(key: string): string | null {
    return this.#items.get(key) ?? null;
  }
  key(index: number): string | null {
    return [...this.#items.keys()][index] ?? null;
  }
  removeItem(key: string): void {
    this.#items.delete(key);
  }
  setItem(key: string, value: string): void {
    this.#items.set(key, value);
  }
}

describe("VaultStore", () => {
  it("shows identities only while unlocked", async () => {
    const store = new VaultStore(fakeVault());
    await store.refresh();
    expect(store.getSnapshot().status?.exists).toBe(false);
    await store.create("correct horse");
    await store.saveIdentity({ name: "Deploy", user: "deploy", password: "hunter2" });
    expect(store.getSnapshot().identities.map((i) => [i.name, i.hasPassword])).toEqual([
      ["Deploy", true],
    ]);
    await store.lock();
    expect(store.getSnapshot()).toMatchObject({ status: { unlocked: false }, identities: [] });
    await expect(store.unlock("wrong one")).rejects.toThrow("the password is wrong");
    await store.unlock("correct horse");
    expect(store.getSnapshot().identities).toHaveLength(1);
  });

  it("keeps secrets out of what it holds", async () => {
    const store = new VaultStore(fakeVault());
    await store.create("correct horse");
    await store.saveIdentity({ name: "k", password: "hunter2", privateKey: "PRIVATE" });
    const held = JSON.stringify(store.getSnapshot());
    expect(held).not.toContain("hunter2");
    expect(held).not.toContain("PRIVATE");
  });

  it("touches the backend at most once per interval", async () => {
    const client = fakeVault();
    const touch = vi.spyOn(client, "touch");
    let now = 0;
    const store = new VaultStore(client, () => now);
    store.activity();
    expect(touch).not.toHaveBeenCalled();
    await store.create("correct horse");
    store.activity();
    store.activity();
    now += TOUCH_EVERY_MS - 1;
    store.activity();
    now += 1;
    store.activity();
    expect(touch).toHaveBeenCalledTimes(2);
  });

  it("does not notify when a refresh finds nothing new", async () => {
    const store = new VaultStore(fakeVault());
    await store.create("correct horse");
    await store.saveIdentity({ name: "Deploy" });
    const listener = vi.fn<() => void>();
    store.subscribe(listener);
    const before = store.getSnapshot();
    await store.refresh();
    await store.refresh();
    expect(listener).not.toHaveBeenCalled();
    expect(store.getSnapshot()).toBe(before);
    await store.saveIdentity({ name: "Other" });
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it("remembers the idle time across runs", async () => {
    const fake = new MemoryStorage();
    const first = new VaultStore(fakeVault(), undefined, fake);
    await first.refresh();
    await first.setIdle(60);
    const client = fakeVault();
    const second = new VaultStore(client, undefined, fake);
    await second.refresh();
    expect(second.getSnapshot().status?.idleMinutes).toBe(60);
  });

  it("touches often enough for a short idle time", async () => {
    const client = fakeVault();
    const touch = vi.spyOn(client, "touch");
    let now = 0;
    const store = new VaultStore(client, () => now);
    await store.create("correct horse");
    await store.setIdle(1);
    store.activity();
    now += 15_000;
    store.activity();
    expect(touch).toHaveBeenCalledTimes(2);
  });

  it("retries at once when another pane already unlocked the vault", async () => {
    const client = fakeVault();
    const store = new VaultStore(client);
    await store.create("correct horse");
    await store.lock();
    const retry = vi.fn<() => void>();
    const ask = vi.fn<() => void>();
    await store.retryOrAsk(retry, ask);
    expect([retry.mock.calls.length, ask.mock.calls.length]).toEqual([0, 1]);
    // Unlocked elsewhere: this store's snapshot still says locked.
    await client.unlock("correct horse");
    await store.retryOrAsk(retry, ask);
    expect([retry.mock.calls.length, ask.mock.calls.length]).toEqual([1, 1]);
  });

  it("shows the vault locked after a change that failed and locked it", async () => {
    const client = fakeVault();
    const store = new VaultStore(client);
    await store.create("correct horse");
    vi.spyOn(client, "saveIdentity").mockImplementation(async () => {
      await client.lock();
      throw new Error("the vault changed in another window");
    });
    await expect(store.saveIdentity({ name: "x" })).rejects.toThrow("another window");
    expect(store.getSnapshot().status?.unlocked).toBe(false);
  });
});
