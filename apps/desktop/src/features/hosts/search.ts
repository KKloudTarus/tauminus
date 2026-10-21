// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { HostView } from "@/ipc/index.ts";

export interface Match {
  host: HostView;
  score: number;
}

/** One searchable field, lowercased, with the letters it contains. */
interface Field {
  readonly text: string;
  /** See [`lettersOf`]. */
  readonly letters: number;
}

/** A host's own searchable text: label, address, user, and tags. */
type Haystack = readonly Field[];

/**
 * Built once per host object. The list is replaced on every change, so a
 * stale entry is never read. The group name is not cached: renaming a group
 * leaves its hosts' objects as they were.
 */
const haystacks = new WeakMap<HostView, Haystack>();

/**
 * Which characters `text` holds, as 32 bits: one per letter a to z, five
 * shared by the digits, one for everything else. Lowercase input only.
 */
function lettersOf(text: string): number {
  let letters = 0;
  for (const char of text) {
    const code = char.codePointAt(0) ?? 0;
    if (code >= 97 && code <= 122) letters |= 1 << (code - 97);
    else if (code >= 48 && code <= 57) letters |= 1 << (26 + ((code - 48) % 5));
    else letters |= 1 << 31;
  }
  return letters;
}

function fieldOf(text: string): Field {
  const lower = text.toLowerCase();
  return { text: lower, letters: lettersOf(lower) };
}

function haystackOf(host: HostView): Haystack {
  let fields = haystacks.get(host);
  if (!fields) {
    fields = [
      host.label,
      host.address,
      host.target?.destination.user ?? host.settings.user ?? "",
      ...host.tags,
    ].map(fieldOf);
    haystacks.set(host, fields);
  }
  return fields;
}

/** Shorter words would match letters scattered across most names. */
const MIN_SCATTERED = 3;

interface Token {
  readonly text: string;
  readonly codePoints: readonly string[];
  readonly letters: number;
}

const WORD_START = /[\s._@-]/;

/**
 * Score of `token` against one field, 0 when it does not match: 6 for the
 * whole field, 4 for a prefix, 3 for the start of a word, 2 for elsewhere,
 * 1 for letters in order. A field missing one of the token's letters is
 * rejected by its letter mask before any scan.
 */
function fieldScore(token: Token, field: Field): number {
  if ((token.letters & ~field.letters) !== 0) return 0;
  const { text } = field;
  let at = text.indexOf(token.text);
  if (at === 0) return token.text.length === text.length ? 6 : 4;
  if (at > 0) {
    // Any occurrence may start a word, not only the first.
    while (at > 0) {
      if (WORD_START.test(text[at - 1] ?? "")) return 3;
      at = text.indexOf(token.text, at + 1);
    }
    return 2;
  }
  // In order but not together: `pdb` finds `prod-db`. Compared by code
  // point, so characters outside the BMP count once.
  if (token.codePoints.length < MIN_SCATTERED) return 0;
  let i = 0;
  for (const char of text) {
    if (char === token.codePoints[i]) i += 1;
    if (i === token.codePoints.length) return 1;
  }
  return 0;
}

/**
 * Hosts matching every word of `query`, best first. Each word must match
 * some field, as a substring or, scoring lower and from three characters
 * on, as letters in order. Ties keep the label order. O(H · T · L) for H
 * hosts, T words and L characters per host, with most non-matching fields
 * rejected in O(1) by their letter masks; haystacks are built once per host
 * object.
 */
export function searchHosts(
  hosts: readonly HostView[],
  query: string,
  groupName: (host: HostView) => string,
): Match[] {
  const tokens: Token[] = query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .map((text) => ({ text, codePoints: Array.from(text), letters: lettersOf(text) }));
  if (tokens.length === 0) return hosts.map((host) => ({ host, score: 0 }));
  const matches: Match[] = [];
  for (const host of hosts) {
    const fields = haystackOf(host);
    const group = fieldOf(groupName(host));
    let score = 0;
    for (const token of tokens) {
      let best = fieldScore(token, group);
      for (const field of fields) best = Math.max(best, fieldScore(token, field));
      if (best === 0) {
        score = 0;
        break;
      }
      score += best;
    }
    if (score > 0) matches.push({ host, score });
  }
  return matches.toSorted((a, b) => b.score - a.score || a.host.label.localeCompare(b.host.label));
}
