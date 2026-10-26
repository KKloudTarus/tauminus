// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Updates for end-to-end tests. Only the `e2e` build loads it. A test that
// wants an update offered sets `tauminus.e2e.update` in local storage
// before the page loads; without it this version is the newest.

import type { UpdatesClient } from "./updates.ts";

export function fakeUpdates(storage: Storage | undefined): UpdatesClient {
  return {
    check: async () => {
      if (storage?.getItem("tauminus.e2e.update") !== "1") return null;
      return { version: "9.9.9", current: "0.0.0", date: null, notes: "Faster sync.\nFewer bugs." };
    },
    install: async (progress) => {
      progress(40);
      progress(100);
      storage?.setItem("tauminus.e2e.installed", "9.9.9");
    },
  };
}
