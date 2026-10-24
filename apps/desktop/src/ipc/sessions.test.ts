// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import type { AnswerSpec, PromptRequest } from "./bindings.ts";

const replies = vi.hoisted(() => [] as [number, AnswerSpec][]);

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {
    onmessage: (message: unknown) => void = () => {};
  },
}));

vi.mock("./bindings.ts", () => ({
  commands: {
    sshPromptReply: (id: number, answer: AnswerSpec) => {
      replies.push([id, answer]);
      return Promise.resolve(true);
    },
  },
}));

const { promptChannel } = await import("./sessions.ts");

const TOUCH: PromptRequest = {
  id: 7,
  prompt: { kind: "securityKeyTouch", keyPath: "~/.ssh/id_ed25519_sk" },
};

describe("promptChannel", () => {
  it("aborts a withdrawn question and sends no answer for it", async () => {
    replies.length = 0;
    let seen: AbortSignal | undefined;
    const channel = promptChannel(
      (_, withdrawn) =>
        new Promise<AnswerSpec>((resolve) => {
          seen = withdrawn;
          withdrawn?.addEventListener("abort", () => resolve({ kind: "cancel" }));
        }),
    );
    channel.onmessage(TOUCH);
    expect(seen?.aborted).toBe(false);
    channel.onmessage({ id: 7, prompt: { kind: "withdrawn" } });
    expect(seen?.aborted).toBe(true);
    await vi.waitFor(() => expect(seen?.aborted).toBe(true));
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(replies).toEqual([]);

    // A withdrawal for a question it never had changes nothing.
    channel.onmessage({ id: 99, prompt: { kind: "withdrawn" } });
    expect(replies).toEqual([]);
  });

  it("sends an answered question's answer, and a cancel when asking fails", async () => {
    replies.length = 0;
    const channel = promptChannel(async (prompt) =>
      prompt.kind === "password" ? { kind: "secret", value: "pw" } : Promise.reject(new Error()),
    );
    channel.onmessage({ id: 1, prompt: { kind: "password", endpoint: "a@db" } });
    channel.onmessage(TOUCH);
    await vi.waitFor(() => expect(replies).toHaveLength(2));
    expect(replies).toContainEqual([1, { kind: "secret", value: "pw" }]);
    expect(replies).toContainEqual([7, { kind: "cancel" }]);
    // Withdrawn after it was answered: nothing more is sent.
    channel.onmessage({ id: 1, prompt: { kind: "withdrawn" } });
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(replies).toHaveLength(2);
  });
});
