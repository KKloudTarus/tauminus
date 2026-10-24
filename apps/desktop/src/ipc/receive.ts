// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import { type ReceiveView, commands } from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { ReceiveView };

/**
 * Files a host sends with `sz` (ADR 0025). The backend finds the offer in
 * a session's output and sends nothing back until `answer`; the files go
 * to `Downloads/Tauminus`, never over one there.
 */
export interface ReceiveClient {
  /** Where every session's transfer events go; called once. */
  listen(handler: (event: ReceiveView) => void): Promise<void>;
  /** The answer to offer number `offer`; the backend ignores any other. */
  answer(session: number, offer: number, accept: boolean): Promise<void>;
  /** Stops the transfer, or declines the offer. */
  cancel(session: number): Promise<void>;
  /** Opens the folder files go to. */
  reveal(): Promise<void>;
}

type Result = Promise<
  | { status: "ok"; data: null }
  | { status: "error"; error: { code: SessionCommandError["code"]; message: string } }
>;

async function done(result: Result): Promise<void> {
  const outcome = await result;
  if (outcome.status === "error") throw new SessionCommandError(outcome.error);
}

export const tauriReceive: ReceiveClient = {
  async listen(handler) {
    const channel = new Channel<ReceiveView>();
    channel.onmessage = handler;
    await commands.receiveListen(channel);
  },
  answer: (session, offer, accept) => done(commands.receiveAnswer(session, offer, accept)),
  cancel: (session) => done(commands.receiveCancel(session)),
  reveal: () => done(commands.receiveReveal()),
};
