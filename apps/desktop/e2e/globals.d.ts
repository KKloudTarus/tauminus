// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// What the e2e build exposes on `window` for tests; see src/ipc/fake-links.ts.
// A script file, so this merges into the global Window type.
interface Window {
  tauminusOpenedLinks?: string[];
}
