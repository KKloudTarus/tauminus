// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { defineConfig, devices } from "@playwright/test";

// End-to-end tests drive the real UI in Chromium against the in-memory shell
// (`vite --mode e2e`). The Tauri backend has its own Rust tests.
export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  forbidOnly: !!process.env["CI"],
  retries: 0,
  reporter: process.env["CI"] ? [["github"], ["list"]] : "list",
  use: {
    baseURL: "http://localhost:1421",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1200, height: 760 } },
    },
  ],
  webServer: {
    command: "pnpm exec vite --mode e2e --port 1421 --strictPort",
    url: "http://localhost:1421",
    reuseExistingServer: !process.env["CI"],
    timeout: 60_000,
  },
});
