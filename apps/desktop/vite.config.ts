// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Port and watch settings follow the Tauri Vite template: the Tauri CLI
// expects a fixed dev server port and rebuilds Rust on its own.
export default defineConfig({
  plugins: [react(), babel({ presets: [reactCompilerPreset()] }), tailwindcss()],
  resolve: { tsconfigPaths: true },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    sourcemap: true,
    // A size budget rather than a silenced warning: the app loads from disk,
    // so the cost is parse time, and growth past 800 kB should be noticed.
    chunkSizeWarningLimit: 800,
  },
});
