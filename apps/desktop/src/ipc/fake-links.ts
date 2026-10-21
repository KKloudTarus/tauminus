// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Records links instead of opening them, for end-to-end tests. Only the
// `e2e` build loads it.

import type { LinkOpener } from "./links.ts";

declare global {
  interface Window {
    /** Links the app asked to open, in order. */
    tauminusOpenedLinks?: string[];
  }
}

export const fakeLinks: LinkOpener = {
  async open(url) {
    window.tauminusOpenedLinks = [...(window.tauminusOpenedLinks ?? []), url];
  },
};
