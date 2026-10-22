// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { commands } from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

/** Text on the OS clipboard. */
export type TextClipboard = Pick<Clipboard, "readText" | "writeText">;

/**
 * The OS clipboard through the backend. WebKitGTK refuses
 * `navigator.clipboard` reads, so paste reached no terminal on Linux.
 */
export const tauriClipboard: TextClipboard = {
  async readText() {
    const result = await commands.clipboardReadText();
    if (result.status === "error") throw new SessionCommandError(result.error);
    return result.data;
  },
  async writeText(text) {
    const result = await commands.clipboardWriteText(text);
    if (result.status === "error") throw new SessionCommandError(result.error);
  },
};
