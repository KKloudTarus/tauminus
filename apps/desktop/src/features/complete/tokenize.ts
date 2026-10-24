// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** A line being typed, read for completion: lenient, never throws. */
export interface TypedLine {
  /** The words of the command the cursor is in, before the current word. */
  words: string[];
  /** The word under the cursor, as far as it is typed, unquoted. */
  current: string;
  /** The current word opens a quote or has an escape: completing it would
   * need quoting, so nothing is offered. */
  quoted: boolean;
}

/** Words after which a new command starts. */
const SEPARATORS = new Set([";", "&&", "||", "|", "&", "(", "$(", "`", "{", "!"]);

/**
 * Splits the text before the cursor into words, starting over after each
 * separator, so `cd /tmp && git ch` reads as `git ch`. Quotes and escapes
 * are understood well enough to keep a quoted space in its word. O(n).
 */
export function tokenize(text: string): TypedLine {
  let words: string[] = [];
  let word = "";
  let inWord = false;
  let quote: "'" | '"' | undefined;
  let quoted = false;
  const end = () => {
    if (inWord) {
      if (SEPARATORS.has(word) && !quoted) words = [];
      else words.push(word);
    }
    word = "";
    inWord = false;
    quoted = false;
  };
  for (let i = 0; i < text.length; i += 1) {
    const c = text.charAt(i);
    if (quote) {
      if (c === quote) quote = undefined;
      else if (c === "\\" && quote === '"' && i + 1 < text.length) word += text.charAt(++i);
      else word += c;
      continue;
    }
    if (c === "'" || c === '"') {
      quote = c;
      quoted = true;
      inWord = true;
    } else if (c === "\\") {
      quoted = true;
      inWord = true;
      if (i + 1 < text.length) word += text.charAt(++i);
    } else if (c === " " || c === "\t") {
      end();
    } else if (c === "&" && (text.charAt(i - 1) === ">" || text.charAt(i + 1) === ">")) {
      // `2>&1`, `&>file`: a redirection, not a separator.
      word += c;
      inWord = true;
    } else if (c === ";" || c === "|" || c === "&" || c === "(" || c === "`") {
      // `&&`, `||`, `$(`: one separator, and the next command starts.
      if (c === "(" && word === "$") {
        word = "";
        inWord = false;
      }
      end();
      words = [];
      if ((c === "&" || c === "|") && text.charAt(i + 1) === c) i += 1;
    } else {
      word += c;
      inWord = true;
    }
  }
  const current = word;
  const open = quote !== undefined || quoted;
  return { words, current, quoted: open };
}
