// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import { fakeFiles } from "@/ipc/fake-files.ts";

import { FilesStore, joinPath, modeText, parseMode, sorted } from "./files-store.ts";

async function opened(): Promise<FilesStore> {
  const store = new FilesStore(fakeFiles(), "db", () => {});
  await store.open();
  return store;
}

describe("FilesStore", () => {
  it("opens on both home folders, folders first", async () => {
    const store = await opened();
    const { local, remote, status } = store.getSnapshot();
    expect(status).toBe("open");
    expect(local.path).toBe("/home/me");
    expect(local.entries.map((e) => e.name)).toEqual(["photos", "notes.txt"]);
    expect(remote.path).toBe("/home/ops");
    expect(remote.parent).toBe("/home");
  });

  it("downloads the selected file and refreshes the local folder", async () => {
    vi.useFakeTimers();
    try {
      const store = await opened();
      store.select("remote", "app.log");
      await store.download();
      expect(store.getSnapshot().transfers[0]?.name).toBe("app.log");
      await vi.advanceTimersByTimeAsync(2000);
      const { transfers, local } = store.getSnapshot();
      expect(transfers[0]?.status.kind).toBe("done");
      expect(local.entries.map((e) => e.name)).toContain("app.log");
    } finally {
      vi.useRealTimers();
    }
  });

  it("says why a file could not be queued", async () => {
    const store = await opened();
    store.select("local", "notes.txt");
    await store.upload();
    // The fake lands the file after a few 100 ms ticks.
    await new Promise((resolve) => setTimeout(resolve, 600));
    await store.upload();
    expect(store.getSnapshot().notice).toMatch(/notes\.txt: .*already exists/);
    store.setOverwrite(true);
    await store.upload();
    expect(store.getSnapshot().transfers).toHaveLength(2);
  });

  it("changes files on the server and lists the folder again", async () => {
    const store = await opened();
    expect(await store.change({ kind: "mkdir", path: "/home/ops/new" })).toBe(true);
    expect(store.getSnapshot().remote.entries.map((e) => e.name)).toContain("new");
    expect(await store.change({ kind: "mkdir", path: "/home/ops/new" })).toBe(false);
    expect(store.getSnapshot().remote.error).toMatch(/exists/);
  });

  it("selects one entry, or toggles with a modifier", async () => {
    const store = await opened();
    store.select("local", "photos");
    store.select("local", "notes.txt", true);
    expect([...store.getSnapshot().local.selected]).toEqual(["photos", "notes.txt"]);
    store.select("local", "photos", true);
    expect([...store.getSnapshot().local.selected]).toEqual(["notes.txt"]);
    store.select("local", "photos");
    expect([...store.getSnapshot().local.selected]).toEqual(["photos"]);
  });

  it("a failed open says why", async () => {
    const store = new FilesStore(fakeFiles(), "", () => {});
    await store.open();
    expect(store.getSnapshot()).toMatchObject({ status: "failed", error: "no host" });
  });
});

function entry(name: string, kind: "dir" | "file") {
  return { name, kind, size: 0, modified: null, mode: null };
}

describe("watching transfers", () => {
  it("a transfer queued while a read is under way still shows", async () => {
    vi.useFakeTimers();
    try {
      const inner = fakeFiles();
      let hold: (() => void) | undefined;
      const client = {
        ...inner,
        // Holds the first read that finds everything finished.
        transfers: async () => {
          const now = await inner.transfers();
          if (hold === undefined && now.length > 0 && now.every((t) => t.status.kind === "done")) {
            await new Promise<void>((resolve) => {
              hold = resolve;
            });
          }
          return now;
        },
      };
      const store = new FilesStore(client, "db", () => {});
      await store.open();
      store.select("remote", "app.log");
      await store.download();
      await vi.waitFor(async () => {
        await vi.advanceTimersByTimeAsync(100);
        if (hold === undefined) throw new Error("not held yet");
      });
      await store.goRemote("/home/ops/deploy");
      store.select("remote", "run.sh");
      await store.download();
      hold?.();
      await vi.advanceTimersByTimeAsync(5000);
      const names = store.getSnapshot().transfers.map((t) => [t.name, t.status.kind]);
      expect(names).toEqual([
        ["app.log", "done"],
        ["run.sh", "done"],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("helpers", () => {
  it("joins paths with the separator in use", () => {
    expect(joinPath("/srv", "a")).toBe("/srv/a");
    expect(joinPath("/", "a")).toBe("/a");
    expect(joinPath("C:\\Users\\me", "a")).toBe("C:\\Users\\me\\a");
  });

  it("reads and writes modes as chmod does", () => {
    expect(parseMode("644")).toBe(0o644);
    expect(parseMode("0755")).toBe(0o755);
    expect(parseMode("4755")).toBe(0o4755);
    for (const bad of ["", "9", "abc", "77777", "-1"]) expect(parseMode(bad)).toBeUndefined();
    expect(modeText(0o754)).toBe("rwxr-xr--");
    expect(modeText(undefined)).toBe("");
  });

  it("lists folders before files", () => {
    expect(
      sorted([entry("b", "file"), entry("z", "dir"), entry("a", "file")]).map((e) => e.name),
    ).toEqual(["z", "a", "b"]);
  });
});

describe("editing", () => {
  it("lists an open edit until it is done", async () => {
    const store = await opened();
    store.select("remote", "app.log");
    await store.edit();
    expect(store.getSnapshot().edits.map((e) => [e.remote, e.state.kind])).toEqual([
      ["/home/ops/app.log", "watching"],
    ]);
    await store.editDone(store.getSnapshot().edits[0]?.id ?? 0);
    expect(store.getSnapshot().edits).toEqual([]);
  });

  it("says why a folder cannot be edited", async () => {
    const store = await opened();
    store.select("remote", "deploy");
    await store.edit();
    expect(store.getSnapshot().notice).toMatch(/deploy: only a file can be edited/);
  });
});
