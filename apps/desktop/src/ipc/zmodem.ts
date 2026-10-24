// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import { type ZmodemView, commands } from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { ZmodemView };

/**
 * Zmodem in a session. The backend finds a host's offer in the output and
 * sends nothing back until the user answers: files `sz` sends go to
 * `Downloads/Tauminus`, never over one there (ADR 0025); files for `rz`
 * are chosen in the system's dialog, which the backend opens, so no path
 * goes through the view (ADR 0026).
 */
export interface ZmodemClient {
  /** Where every session's transfer events go; called once. */
  listen(handler: (event: ZmodemView) => void): Promise<void>;
  /** The answer to offer number `offer` from `sz`; the backend ignores any other. */
  receive(session: number, offer: number, accept: boolean): Promise<void>;
  /** Opens the file dialog for offer `offer` from `rz`; false when nothing was chosen. */
  choose(session: number, offer: number): Promise<boolean>;
  /** Stops the transfer, or declines the offer. */
  cancel(session: number): Promise<void>;
  /** Opens the folder received files go to. */
  reveal(): Promise<void>;
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

export const tauriZmodem: ZmodemClient = {
  async listen(handler) {
    const channel = new Channel<ZmodemView>();
    channel.onmessage = handler;
    await commands.zmodemListen(channel);
  },
  receive: async (session, offer, accept) => {
    await unwrap(commands.zmodemReceive(session, offer, accept));
  },
  choose: (session, offer) => unwrap(commands.zmodemChoose(session, offer)),
  cancel: async (session) => {
    await unwrap(commands.zmodemCancel(session));
  },
  reveal: async () => {
    await unwrap(commands.zmodemReveal());
  },
};
