// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { fakeSnippets } from "@/ipc/fake-snippets.ts";
import type { SnippetView } from "@/ipc/index.ts";

import { SnippetsStore, byFolder, snippetTitle } from "./snippets-store.ts";

const snippet = (name: string, folder: string | null): SnippetView => ({
  id: name,
  name,
  folder,
  body: "true",
  variables: [],
});

describe("SnippetsStore", () => {
  it("keeps the list the backend returns after each change", async () => {
    const store = new SnippetsStore(fakeSnippets(() => []));
    await store.load();
    expect(store.getSnapshot()).toMatchObject({ loaded: true, snippets: [] });
    expect(await store.save({ id: null, name: "logs", folder: "Ops", body: "tail {{f}}" })).toBe(
      true,
    );
    const [saved] = store.getSnapshot().snippets;
    expect(saved?.variables).toEqual([{ name: "f", default: null }]);
    expect(store.find(saved?.id ?? "")).toBe(saved);
    expect(await store.remove(saved?.id ?? "")).toBe(true);
    expect(store.getSnapshot().snippets).toEqual([]);
  });

  it("says why a change failed and keeps the list", async () => {
    const store = new SnippetsStore(fakeSnippets(() => []));
    await store.save({ id: null, name: "a", folder: null, body: "true" });
    expect(await store.save({ id: null, name: "b", folder: null, body: "  " })).toBe(false);
    expect(store.getSnapshot().error).toBe("a snippet needs a command");
    expect(store.getSnapshot().snippets.map((s) => s.name)).toEqual(["a"]);
  });
});

describe("byFolder", () => {
  it("groups a sorted list, unfiled first", () => {
    const groups = byFolder([
      snippet("a", null),
      snippet("b", "Logs"),
      snippet("c", "Logs"),
      snippet("d", "Ops"),
    ]);
    expect(groups.map((g) => [g.name, g.snippets.map((s) => s.name)])).toEqual([
      [null, ["a"]],
      ["Logs", ["b", "c"]],
      ["Ops", ["d"]],
    ]);
    expect(snippetTitle(snippet("b", "Logs"))).toBe("Logs / b");
  });
});

describe("fakeSnippets", () => {
  it("types startup snippets with their defaults and skips the rest", async () => {
    const client = fakeSnippets(() => ["snippet-1", "snippet-2", "gone"]);
    await client.save({ id: null, name: "tmux", folder: null, body: "tmux new -A -s {{s:main}}" });
    await client.save({ id: null, name: "cd", folder: null, body: "cd {{dir}}" });
    const { text, skipped } = await client.startup("db");
    expect(text).toBe("tmux new -A -s main\r");
    expect(skipped.map((s) => s.id)).toEqual(["snippet-2", "gone"]);
  });
});
