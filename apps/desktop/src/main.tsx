// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/app/App.tsx";
import { AppShell } from "@/app/shell.ts";
import { sessionClient } from "@/ipc/index.ts";
import "@/app/styles.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("index.html is missing #root");
}

// Created outside React so StrictMode's double mount cannot open or close sessions.
const shell = new AppShell({
  sessions: await sessionClient(),
  userAgent: navigator.userAgent,
  storage: safeLocalStorage(),
  root: document.documentElement,
  clipboard: navigator.clipboard,
  // End-to-end tests read the screen from the DOM renderer.
  renderer: import.meta.env.MODE === "e2e" ? "dom" : "auto",
});
shell.workspace.newTab();

createRoot(root).render(
  <StrictMode>
    <App shell={shell} />
  </StrictMode>,
);

function safeLocalStorage(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}
