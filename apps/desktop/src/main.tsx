// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/app/App.tsx";
import { TerminalController } from "@/features/terminal/controller.ts";
import { tauriSessions } from "@/ipc/index.ts";
import "@/app/styles.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("index.html is missing #root");
}

// Created outside React so StrictMode's double mount cannot open or close it.
const terminal = new TerminalController(tauriSessions);

createRoot(root).render(
  <StrictMode>
    <App terminal={terminal} />
  </StrictMode>,
);
