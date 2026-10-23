// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import { fakeTriggers } from "@/ipc/fake-triggers.ts";
import type { RuleSpec } from "@/ipc/index.ts";

import { TriggersStore, defaults } from "./triggers-store.ts";

const rule: RuleSpec = {
  name: "e",
  pattern: "ERROR",
  enabled: true,
  action: { kind: "highlight", style: { fg: "1" } },
};

describe("TriggersStore", () => {
  it("tells panes after each saved change, not after a load or a refusal", async () => {
    const changed = vi.fn<() => void>();
    const store = new TriggersStore(
      fakeTriggers(() => []),
      changed,
    );
    await store.load();
    expect(changed).not.toHaveBeenCalled();
    expect(await store.save({ name: "Errors", rules: [rule] })).toBe(true);
    expect(changed).toHaveBeenCalledTimes(1);
    expect(await store.save({ name: "Bad", rules: [{ ...rule, pattern: "(" }] })).toBe(false);
    expect(store.getSnapshot().error).toContain("not valid");
    expect(changed).toHaveBeenCalledTimes(1);
  });

  it("keeps the order sets were made defaults in, whatever their names", async () => {
    const store = new TriggersStore(fakeTriggers(() => []));
    await store.save({ name: "Alpha", rules: [rule] });
    await store.save({ name: "Zeta", rules: [rule] });
    await store.save({ name: "Mid", rules: [rule] });
    const id = (name: string) => store.getSnapshot().sets.find((s) => s.name === name)?.id ?? "";
    await store.setDefault(id("Zeta"), true);
    await store.setDefault(id("Alpha"), true);
    await store.setDefault(id("Mid"), true);
    await store.setDefault(id("Alpha"), false);
    expect(defaults(store.getSnapshot().sets)).toEqual([id("Zeta"), id("Mid")]);
  });
});
