// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The static completion specs in `specs/`, converted from withfig/autocomplete
// by `scripts/fig-to-json.mjs`. Short keys keep the files small.

/** A command or subcommand. */
export interface SpecNode {
  /** Its names; the first is the one shown. */
  n: string[];
  d?: string;
  /** Subcommands. */
  s?: SpecNode[];
  o?: SpecOption[];
  a?: SpecArg[];
  /** The rest is this other command's spec (`docker compose`). */
  l?: string;
}

export interface SpecOption {
  n: string[];
  d?: string;
  a?: SpecArg[];
  /** Persistent: subcommands take it too. */
  p?: 1;
  /** Repeatable. */
  r?: 1;
}

export interface SpecArg {
  n?: string;
  d?: string;
  /** Fixed suggestions. */
  s?: { n: string; d?: string }[];
  /** Optional. */
  o?: 1;
  /** Variadic: takes every word left. */
  v?: 1;
  /** A command: the words from here are another command line. */
  c?: 1;
  /** Files (`f`) or folders (`d`), which the pane does not list. */
  t?: "f" | "d";
}
