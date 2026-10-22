// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

const PASTE_START = "\u001b[200~";
const PASTE_END = "\u001b[201~";

/**
 * A bracketed paste with every ESC and CSI removed from inside it. Pasted
 * text that held `ESC [201~` would end the paste early, and the shell would
 * run what follows as typed keys. Applies to every terminal, guarded or not.
 */
export function defusePaste(data: string): string {
  if (!data.startsWith(PASTE_START) || !data.endsWith(PASTE_END)) return data;
  const inner = data.slice(PASTE_START.length, -PASTE_END.length);
  // Every ESC and CSI goes, not only whole markers: removing a marker once
  // would let `ESC [20 ESC[201~ 1~` close up into a new one.
  if (!inner.includes("\u001b") && !inner.includes("\u009b")) return data;
  return PASTE_START + inner.replaceAll("\u001b", "").replaceAll("\u009b", "") + PASTE_END;
}
