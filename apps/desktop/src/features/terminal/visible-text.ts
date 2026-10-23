// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** Characters of a confirmation shown before the rest is summarized. */
export const MAX_SHOWN = 4000;

/**
 * Text to confirm, with nothing hidden: controls other than newline and tab
 * appear as `^X` or `<U+XXXX>`, and so do bidirectional controls, which
 * could make a command read differently than it runs (CVE-2021-42574).
 * Past `MAX_SHOWN` characters the rest is counted, not read: a 1 MiB paste
 * costs as much as 4000 characters. O(min(n, MAX_SHOWN)).
 */
export function visibleText(text: string): string {
  let shown = "";
  let count = 0;
  let units = 0;
  for (const char of text) {
    if (count === MAX_SHOWN) break;
    count += 1;
    units += char.length;
    shown += visible(char);
  }
  // Counted in UTF-16 units past here: exact for most text, a little high
  // for emoji, and never short.
  const rest = text.length - units;
  return rest > 0 ? `${shown}… ${rest} more characters` : shown;
}

function visible(char: string): string {
  const code = char.codePointAt(0) ?? 0;
  if (char === "\n" || char === "\t") return char;
  if (code < 0x20) return `^${String.fromCodePoint(code + 0x40)}`;
  if (code === 0x7f) return "^?";
  return hidden(code) ? `<U+${code.toString(16).toUpperCase().padStart(4, "0")}>` : char;
}

/**
 * C1 controls, bidirectional controls, and characters that render as
 * nothing: zero-width and format characters, fillers, variation selectors,
 * and tags.
 */
function hidden(code: number): boolean {
  return (
    (code >= 0x80 && code <= 0x9f) ||
    code === 0xad ||
    code === 0x34f ||
    code === 0x61c ||
    code === 0x115f ||
    code === 0x1160 ||
    code === 0x180e ||
    (code >= 0x200b && code <= 0x200f) ||
    (code >= 0x2028 && code <= 0x202e) ||
    (code >= 0x2060 && code <= 0x2069) ||
    code === 0x3164 ||
    (code >= 0xfe00 && code <= 0xfe0f) ||
    code === 0xfeff ||
    (code >= 0xfff9 && code <= 0xfffb) ||
    (code >= 0xe0000 && code <= 0xe007f)
  );
}
