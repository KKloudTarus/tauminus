// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { commands } from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

/** Opens web links in the default browser. The backend refuses anything but http and https. */
export interface LinkOpener {
  open(url: string): Promise<void>;
}

export const tauriLinks: LinkOpener = {
  async open(url) {
    const result = await commands.openLink(url);
    if (result.status === "error") throw new SessionCommandError(result.error);
  },
};
