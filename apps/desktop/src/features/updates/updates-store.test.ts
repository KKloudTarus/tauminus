// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import type { UpdateView, UpdatesClient } from "@/ipc/index.ts";

import { UpdatesStore, updateNotice } from "./updates-store.ts";

const DAY = 24 * 60 * 60 * 1000;
const NEW: UpdateView = { version: "0.6.0", current: "0.5.0", date: null, notes: "Faster." };

function memory(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (key) => map.get(key) ?? null,
    key: (at) => [...map.keys()][at] ?? null,
    removeItem: (key) => void map.delete(key),
    setItem: (key, value) => void map.set(key, value),
  };
}

function client(found: UpdateView | null | Error, installs: string[] = []) {
  let checks = 0;
  const updates: UpdatesClient = {
    check: async () => {
      checks += 1;
      if (found instanceof Error) throw found;
      return found;
    },
    install: async () => {
      installs.push("installed");
    },
  };
  return { updates, checks: () => checks };
}

describe("UpdatesStore", () => {
  it("looks at start at most once a day, and never once turned off", async () => {
    const storage = memory();
    let now = 10 * DAY;
    const { updates, checks } = client(null);
    await new UpdatesStore(updates, storage, () => now).startup();
    expect(checks()).toBe(1);
    now += DAY - 1;
    await new UpdatesStore(updates, storage, () => now).startup();
    expect(checks()).toBe(1);
    now += 1;
    const store = new UpdatesStore(updates, storage, () => now);
    await store.startup();
    expect(checks()).toBe(2);
    store.setAuto(false);
    now += 5 * DAY;
    const off = new UpdatesStore(updates, storage, () => now);
    expect(off.getSnapshot().auto).toBe(false);
    await off.startup();
    expect(checks()).toBe(2);
  });

  it("a clock set back counts as a day gone", async () => {
    const storage = memory();
    let now = 10 * DAY;
    const { updates, checks } = client(null);
    await new UpdatesStore(updates, storage, () => now).startup();
    now -= DAY;
    await new UpdatesStore(updates, storage, () => now).startup();
    expect(checks()).toBe(2);
  });

  it("a quiet check says nothing unless it finds a version", async () => {
    const quiet = new UpdatesStore(client(new Error("offline")).updates, memory());
    await quiet.startup();
    expect(quiet.getSnapshot().status).toBe("failed");
    expect(updateNotice(quiet.getSnapshot())).toBeUndefined();
    const current = new UpdatesStore(client(null).updates, memory());
    await current.startup();
    expect(updateNotice(current.getSnapshot())).toBeUndefined();
    const found = new UpdatesStore(client(NEW).updates, memory());
    await found.startup();
    expect(updateNotice(found.getSnapshot())).toBe("Tauminus 0.6.0 is available; this is 0.5.0.");
  });

  it("a check the user asks for always answers", async () => {
    const current = new UpdatesStore(client(null).updates, memory());
    await current.check();
    expect(updateNotice(current.getSnapshot())).toBe("This is the newest version of Tauminus.");
    const failed = new UpdatesStore(client(new Error("offline")).updates, memory());
    await failed.check();
    expect(updateNotice(failed.getSnapshot())).toBe("Could not check for updates: offline");
  });

  it("installs only what a check found, when asked", async () => {
    const installs: string[] = [];
    const store = new UpdatesStore(client(NEW, installs).updates, memory());
    await store.install();
    expect(installs).toEqual([]);
    await store.check(false);
    await store.install();
    expect(installs).toEqual(["installed"]);
    expect(store.getSnapshot().status).toBe("installing");
  });

  it("a failed install is told, and Later hides the offer", async () => {
    const broken: UpdatesClient = {
      check: async () => NEW,
      install: async () => {
        throw new Error("signature does not match");
      },
    };
    const store = new UpdatesStore(broken, memory());
    await store.check(false);
    await store.install();
    expect(updateNotice(store.getSnapshot())).toBe(
      "Could not install the update: signature does not match",
    );
    await store.check(false);
    store.dismiss();
    expect(updateNotice(store.getSnapshot())).toBeUndefined();
  });

  it("a check asked for while the start check runs is answered by it", async () => {
    let answer: ((found: UpdateView | null) => void) | undefined;
    const slow: UpdatesClient = {
      check: () => new Promise((resolve) => (answer = resolve)),
      install: async () => {},
    };
    const store = new UpdatesStore(slow, memory());
    const start = store.startup();
    await store.check();
    expect(updateNotice(store.getSnapshot())).toBe("Checking for updates…");
    answer?.(null);
    await start;
    expect(updateNotice(store.getSnapshot())).toBe("This is the newest version of Tauminus.");
  });

  it("tells how much of the download has come", async () => {
    let step: ((percent: number) => void) | undefined;
    let finish: (() => void) | undefined;
    const store = new UpdatesStore(
      {
        check: async () => NEW,
        install: (progress) =>
          new Promise<void>((resolve) => {
            step = progress;
            finish = resolve;
          }),
      },
      memory(),
    );
    await store.check(false);
    const installing = store.install();
    step?.(42);
    expect(updateNotice(store.getSnapshot())).toBe("Downloading Tauminus 0.6.0: 42%");
    step?.(100);
    expect(updateNotice(store.getSnapshot())).toBe(
      "Installing Tauminus 0.6.0; it restarts when done.",
    );
    finish?.();
    await installing;
  });
});
