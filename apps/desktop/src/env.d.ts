// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

interface ImportMetaEnv {
  /** "1" in a benchmark build; see src/bench/run.ts. */
  readonly VITE_TAUMINUS_BENCH?: string;
  /** "dom" or "webgl" pins the terminal renderer at build time; else auto. */
  readonly VITE_TAUMINUS_RENDERER?: string;
}
