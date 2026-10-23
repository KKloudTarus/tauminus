// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/app/App.tsx";
import { AppShell } from "@/app/shell.ts";
import {
  filesClient,
  forwardsClient,
  historyClient,
  hostsClient,
  linkOpener,
  serialClient,
  sessionClient,
  snippetsClient,
  textClipboard,
  vaultClient,
} from "@/ipc/index.ts";
import "@/app/styles.css";

const root = document.getElementById("root");
if (!root) {
  throw new Error("index.html is missing #root");
}

// Created outside React so StrictMode's double mount cannot open or close sessions.
const shell = new AppShell({
  sessions: await sessionClient(),
  hosts: await hostsClient(),
  vault: await vaultClient(),
  history: await historyClient(),
  forwards: await forwardsClient((host, rule) => ruleLabel(host, rule)),
  files: await filesClient(),
  serial: await serialClient(),
  snippets: await snippetsClient(
    (host) => shell.hosts.getSnapshot().view.hosts.find((h) => h.id === host)?.startup ?? [],
  ),
  links: await linkOpener(),
  userAgent: navigator.userAgent,
  storage: safeLocalStorage(),
  root: document.documentElement,
  clipboard: await textClipboard(),
  // End-to-end tests read the screen from the DOM renderer; a benchmark
  // build may pin one to compare them.
  renderer: import.meta.env.MODE === "e2e" ? "dom" : rendererOverride(),
});
shell.workspace.newTab();
void shell.hosts.load();
void shell.snippets?.load();
// Only a benchmark build has this flag; other builds drop the import.
if (import.meta.env.VITE_TAUMINUS_BENCH === "1") {
  void import("@/bench/run.ts").then(({ runBenchmark }) => runBenchmark(shell));
}

createRoot(root).render(
  <StrictMode>
    <App shell={shell} />
  </StrictMode>,
);

/** A rule's name, for the `e2e` build's fake forwarding. */
function ruleLabel(host: string, rule: string): string {
  const saved = shell.hosts.getSnapshot().view.hosts.find((h) => h.id === host);
  return saved?.forwards.find((r) => r.id === rule)?.label ?? "";
}

function rendererOverride(): "auto" | "dom" | "webgl" {
  const value = import.meta.env.VITE_TAUMINUS_RENDERER;
  return value === "dom" || value === "webgl" ? value : "auto";
}

function safeLocalStorage(): Storage | undefined {
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}
