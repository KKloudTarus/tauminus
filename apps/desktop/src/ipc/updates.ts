// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import { type UpdateView, commands } from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { UpdateView };

/** New versions from the project's releases, signed with its key (ADR 0033). */
export interface UpdatesClient {
  /** A newer version than this one, or `null`. */
  check(): Promise<UpdateView | null>;
  /**
   * Downloads, checks and installs the version `check` last found, then
   * restarts; `progress` hears each whole percent downloaded.
   */
  install(progress: (percent: number) => void): Promise<void>;
}

export const tauriUpdates: UpdatesClient = {
  async check() {
    const result = await commands.updateCheck();
    if (result.status === "error") throw new SessionCommandError(result.error);
    return result.data;
  },
  async install(progress) {
    const channel = new Channel<number>();
    channel.onmessage = progress;
    const result = await commands.updateInstall(channel);
    if (result.status === "error") throw new SessionCommandError(result.error);
  },
};
