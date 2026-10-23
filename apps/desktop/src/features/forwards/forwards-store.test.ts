// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import { fakeForwards } from "@/ipc/fake-forwards.ts";
import { type ForwardsClient, SessionCommandError } from "@/ipc/index.ts";

import { ForwardsStore, ruleKey } from "./forwards-store.ts";

const label = (_host: string, rule: string) => rule;

describe("ForwardsStore", () => {
  it("starts, lists, and stops a rule", async () => {
    const store = new ForwardsStore(fakeForwards(label), () => {});
    await store.toggle("db", "pg");
    expect(store.isRunning("db", "pg")).toBe(true);
    expect(store.getSnapshot().running.get(ruleKey("db", "pg"))?.port).toBe(41_000);
    await store.toggle("db", "pg");
    expect(store.isRunning("db", "pg")).toBe(false);
    expect(store.getSnapshot().busy.size).toBe(0);
  });

  it("keeps the reason a start failed", async () => {
    const store = new ForwardsStore(fakeForwards(label), () => {});
    await store.start("db", "busy");
    expect(store.getSnapshot().errors.get(ruleKey("db", "busy"))).toMatch(/address in use/);
    expect(store.isRunning("db", "busy")).toBe(false);
  });

  it("asks to unlock the vault and tries again", async () => {
    let locked = true;
    const inner = fakeForwards(label);
    const client: ForwardsClient = {
      ...inner,
      start: (host, rule, onPrompt) =>
        locked
          ? Promise.reject(
              new SessionCommandError({ code: "vault_locked", message: "the vault is locked" }),
            )
          : inner.start(host, rule, onPrompt),
    };
    const retries: (() => void)[] = [];
    const store = new ForwardsStore(client, (retry) => retries.push(retry));
    await store.start("db", "pg");
    expect(retries).toHaveLength(1);
    expect(store.getSnapshot().errors.size).toBe(0);
    locked = false;
    retries[0]?.();
    await vi.waitFor(() => expect(store.isRunning("db", "pg")).toBe(true));
  });

  it("shows a prompt and passes the answer back", async () => {
    const inner = fakeForwards(label);
    const client: ForwardsClient = {
      ...inner,
      start: async (host, rule, onPrompt) => {
        const answer = await onPrompt({ kind: "password", endpoint: "ops@db" });
        if (answer.kind !== "secret") throw new Error("expected a password");
        return inner.start(host, rule, onPrompt);
      },
    };
    const store = new ForwardsStore(client, () => {});
    const started = store.start("db", "pg");
    await vi.waitFor(() => expect(store.getSnapshot().prompt).toBeDefined());
    expect(store.getSnapshot().prompt?.prompt).toEqual({ kind: "password", endpoint: "ops@db" });
    store.getSnapshot().prompt?.answer({ kind: "secret", value: "hunter2" });
    await started;
    expect(store.getSnapshot().prompt).toBeUndefined();
    expect(store.isRunning("db", "pg")).toBe(true);
  });

  it("a start that ends leaves another start's question open", async () => {
    const inner = fakeForwards(label);
    const client: ForwardsClient = {
      ...inner,
      start: async (host, rule, onPrompt) => {
        if (host === "slow") {
          const answer = await onPrompt({ kind: "password", endpoint: "ops@slow" });
          if (answer.kind !== "secret") throw new Error("cancelled");
        }
        return inner.start(host, rule, onPrompt);
      },
    };
    const store = new ForwardsStore(client, () => {});
    const slow = store.start("slow", "pg");
    await vi.waitFor(() => expect(store.getSnapshot().prompt).toBeDefined());
    await store.start("fast", "pg");
    expect(store.getSnapshot().prompt?.prompt).toEqual({ kind: "password", endpoint: "ops@slow" });
    store.getSnapshot().prompt?.answer({ kind: "secret", value: "hunter2" });
    await slow;
    expect(store.isRunning("slow", "pg")).toBe(true);
    expect(store.getSnapshot().prompt).toBeUndefined();
  });

  it("questions from two starts are asked in turn", async () => {
    const inner = fakeForwards(label);
    const client: ForwardsClient = {
      ...inner,
      start: async (host, rule, onPrompt) => {
        await onPrompt({ kind: "password", endpoint: host });
        return inner.start(host, rule, onPrompt);
      },
    };
    const store = new ForwardsStore(client, () => {});
    const a = store.start("a", "pg");
    const b = store.start("b", "pg");
    await vi.waitFor(() =>
      expect(store.getSnapshot().prompt?.prompt).toMatchObject({ endpoint: "a" }),
    );
    store.getSnapshot().prompt?.answer({ kind: "secret", value: "x" });
    await vi.waitFor(() =>
      expect(store.getSnapshot().prompt?.prompt).toMatchObject({ endpoint: "b" }),
    );
    store.getSnapshot().prompt?.answer({ kind: "secret", value: "y" });
    await Promise.all([a, b]);
    expect(store.isRunning("a", "pg") && store.isRunning("b", "pg")).toBe(true);
  });

  it("a cancelled login is not an error", async () => {
    const client: ForwardsClient = {
      ...fakeForwards(label),
      start: () =>
        Promise.reject(new SessionCommandError({ code: "cancelled", message: "cancelled" })),
    };
    const store = new ForwardsStore(client, () => {});
    await store.start("db", "pg");
    expect(store.getSnapshot().errors.size).toBe(0);
  });

  it("a status read that changes nothing notifies no one", async () => {
    const store = new ForwardsStore(fakeForwards(label), () => {});
    await store.start("db", "pg");
    const first = store.getSnapshot();
    let notified = 0;
    store.subscribe(() => (notified += 1));
    await store.refresh();
    expect(notified).toBe(0);
    expect(store.getSnapshot()).toBe(first);
    await store.start("db", "redis");
    const kept = store.getSnapshot().running.get(ruleKey("db", "pg"));
    expect(kept).toBe(first.running.get(ruleKey("db", "pg")));
  });

  it("stops a rule while it is still starting", async () => {
    const inner = fakeForwards(label);
    let release: (() => void) | undefined;
    const client: ForwardsClient = {
      ...inner,
      start: async () => {
        await new Promise<void>((resolve) => (release = resolve));
        throw new SessionCommandError({ code: "cancelled", message: "stopped" });
      },
    };
    const stop = vi.spyOn(client, "stop");
    const store = new ForwardsStore(client, () => {});
    const started = store.start("db", "pg");
    await vi.waitFor(() => expect(store.isStarting("db", "pg")).toBe(true));
    await store.toggle("db", "pg");
    expect(stop).toHaveBeenCalledWith("db", "pg");
    release?.();
    await started;
    expect(store.isStarting("db", "pg")).toBe(false);
    expect(store.getSnapshot().errors.size).toBe(0);
  });

  it("reads status only while watched", async () => {
    vi.useFakeTimers();
    try {
      const client = fakeForwards(label);
      const status = vi.spyOn(client, "status");
      const store = new ForwardsStore(client, () => {});
      const stopA = store.watch();
      const stopB = store.watch();
      await vi.advanceTimersByTimeAsync(3000);
      const seen = status.mock.calls.length;
      expect(seen).toBe(4);
      stopA();
      stopA();
      await vi.advanceTimersByTimeAsync(1000);
      expect(status.mock.calls.length).toBe(seen + 1);
      stopB();
      await vi.advanceTimersByTimeAsync(5000);
      expect(status.mock.calls.length).toBe(seen + 1);
    } finally {
      vi.useRealTimers();
    }
  });
});
