// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** Longest server-provided string shown in a prompt. */
const MAX_LENGTH = 1024;

/**
 * Makes text a server sent safe to show: drops C0 and C1 controls except
 * newline and tab, and Unicode bidirectional controls, which can make text
 * read differently than it is (CVE-2021-42574). React escapes markup. O(n).
 */
export function serverText(text: string): string {
  let clean = "";
  for (const char of text) {
    const code = char.codePointAt(0) ?? 0;
    const control =
      (code < 0x20 && code !== 0x0a && code !== 0x09) || (code >= 0x7f && code <= 0x9f);
    const bidi =
      (code >= 0x202a && code <= 0x202e) ||
      (code >= 0x2066 && code <= 0x2069) ||
      code === 0x200e ||
      code === 0x200f ||
      code === 0x061c;
    if (!control && !bidi) clean += char;
    if (clean.length >= MAX_LENGTH) break;
  }
  return clean.trim();
}
