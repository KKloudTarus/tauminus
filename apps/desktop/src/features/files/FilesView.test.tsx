// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

// The layout that bounds the list comes from the app's styles.
import "@/app/styles.css";
import { fakeFiles } from "@/ipc/fake-files.ts";
import type { FilesClient } from "@/ipc/index.ts";

import { FilesStore } from "./files-store.ts";
import { FilesView, sizeText } from "./FilesView.tsx";

const MANY = 5000;

/** A server folder of `MANY` files. */
function bigFolder(): FilesClient {
  const inner = fakeFiles();
  return {
    ...inner,
    list: (_id, path) =>
      Promise.resolve({
        path,
        parent: "/",
        entries: Array.from({ length: MANY }, (_, i) => ({
          name: `file-${String(i).padStart(5, "0")}`,
          kind: "file" as const,
          size: i,
          modified: 0,
          mode: 0o644,
        })),
      }),
  };
}

describe("FilesView", () => {
  it("draws a screenful of a big folder and the rest as it scrolls", async () => {
    const client = bigFolder();
    const store = new FilesStore(client, "db", () => {});
    await store.open();
    const screen = await render(
      <div style={{ height: 600 }}>
        <FilesView store={store} client={client} hostLabel="db" onClose={() => {}} />
      </div>,
    );
    const list = screen.getByRole("list", { name: "db entries" });
    await expect.element(list).toBeInTheDocument();
    const drawn = list.element().querySelectorAll("li").length;
    expect(drawn).toBeGreaterThan(10);
    expect(drawn).toBeLessThan(100);
    const scroller = list.element().parentElement;
    if (!scroller) throw new Error("no scroller");
    scroller.scrollTop = scroller.scrollHeight;
    scroller.dispatchEvent(new Event("scroll"));
    const lastName = `file-${String(MANY - 1).padStart(5, "0")}`;
    await expect
      .poll(() =>
        [...list.element().querySelectorAll("li")].some((li) => li.textContent?.includes(lastName)),
      )
      .toBe(true);
    expect(list.element().querySelectorAll("li").length).toBeLessThan(100);
  });
});

describe("FilesView rows", () => {
  it("Enter on a row downloads that row, not the earlier selection", async () => {
    const inner = bigFolder();
    const asked: string[] = [];
    const client: FilesClient = {
      ...inner,
      download: (id, remote, into, overwrite) => {
        asked.push(remote);
        return inner.download(id, remote, into, overwrite);
      },
    };
    const store = new FilesStore(client, "db", () => {});
    await store.open();
    store.select("remote", "file-00000");
    const screen = await render(
      <div style={{ height: 600 }}>
        <FilesView store={store} client={client} hostLabel="db" onClose={() => {}} />
      </div>,
    );
    const row = screen.getByRole("button", { name: /file-00003/ });
    await expect.element(row).toBeInTheDocument();
    row.element().dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await expect.poll(() => asked).toEqual(["/home/ops/file-00003"]);
  });

  it("a new folder opens at its top", async () => {
    const client = bigFolder();
    const store = new FilesStore(client, "db", () => {});
    await store.open();
    const screen = await render(
      <div style={{ height: 600 }}>
        <FilesView store={store} client={client} hostLabel="db" onClose={() => {}} />
      </div>,
    );
    const list = screen.getByRole("list", { name: "db entries" });
    await expect.element(list).toBeInTheDocument();
    const scroller = list.element().parentElement;
    if (!scroller) throw new Error("no scroller");
    scroller.scrollTop = 40_000;
    scroller.dispatchEvent(new Event("scroll"));
    await store.goRemote("/other");
    await expect.poll(() => list.element().parentElement?.scrollTop).toBe(0);
    await expect
      .poll(() => list.element().querySelector("li")?.textContent?.includes("file-00000"))
      .toBe(true);
  });
});

describe("sizeText", () => {
  it("writes sizes as file managers do", () => {
    expect(sizeText(999)).toBe("999 B");
    expect(sizeText(1500)).toBe("1.5 kB");
    expect(sizeText(25_000_000)).toBe("25 MB");
  });
});
