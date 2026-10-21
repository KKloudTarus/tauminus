// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** The backend's limit; checked here too, so a refused link costs no IPC. */
const MAX_URL = 2048;

/**
 * Whether a link from terminal output is one Tauminus opens: http or https,
 * a host, and only printable ASCII. The backend checks again and decides.
 */
export function isOpenableLink(url: string): boolean {
  if (url.length > MAX_URL || !/^[\x21-\x7e]+$/.test(url)) return false;
  const match = /^https?:\/\/(.)/i.exec(url);
  return match !== null && !"/?#".includes(match[1] ?? "/");
}

/**
 * Whether a click asks to open a link: Ctrl+click, or Cmd+click on macOS.
 * A plain click stays a click, so selecting text never opens a page.
 */
export function asksToOpen(event: Pick<MouseEvent, "ctrlKey" | "metaKey">, mac: boolean): boolean {
  return mac ? event.metaKey : event.ctrlKey;
}
