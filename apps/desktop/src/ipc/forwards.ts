// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type ForwardKindSpec, type ForwardSpec, type ForwardState, commands } from "./bindings.ts";
import { type PromptHandler, SessionCommandError, promptChannel } from "./sessions.ts";

export type { ForwardKindSpec, ForwardSpec, ForwardState };

/** Saved forwarding rules: start, stop, and what runs. */
export interface ForwardsClient {
  /** Every rule that ran or failed since the app started. */
  status(): Promise<ForwardState[]>;
  /** Starts a saved rule and returns the port it listens on. */
  start(host: string, rule: string, onPrompt: PromptHandler): Promise<number>;
  /** False when the rule was not running. */
  stop(host: string, rule: string): Promise<boolean>;
}

type Result<T> = Promise<
  | { status: "ok"; data: T }
  | { status: "error"; error: { code: SessionCommandError["code"]; message: string } }
>;

async function unwrap<T>(result: Result<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") throw new SessionCommandError(outcome.error);
  return outcome.data;
}

export const tauriForwards: ForwardsClient = {
  status: () => commands.forwardsStatus(),
  start: (host, rule, onPrompt) => {
    const prompts = promptChannel(onPrompt);
    return unwrap(commands.forwardStart(host, rule, prompts));
  },
  stop: (host, rule) => unwrap(commands.forwardStop(host, rule)),
};
