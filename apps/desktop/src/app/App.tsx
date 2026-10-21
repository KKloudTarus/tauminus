// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useSyncExternalStore } from "react";

import { displayChord } from "@/features/keymap/keymap.ts";
import { ConnectDialog } from "@/features/ssh/ConnectDialog.tsx";
import { PaneTree } from "@/features/workspace/PaneTree.tsx";
import { TabBar } from "@/features/workspace/TabBar.tsx";

import { CommandPalette } from "./CommandPalette.tsx";
import type { AppShell } from "./shell.ts";

export function App({ shell }: { shell: AppShell }) {
  const { workspace } = shell;
  const { tabs, activeTabId } = useSyncExternalStore(workspace.subscribe, workspace.getSnapshot);
  const active = tabs.find((tab) => tab.id === activeTabId);

  // Shortcuts that arrive while focus is outside a terminal, such as on the tab bar.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const { paletteOpen, connectOpen } = shell.getState();
      if (!event.defaultPrevented && !paletteOpen && !connectOpen) shell.handleKey(event);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [shell]);

  const chord = (action: Parameters<typeof shell.keymap.chords>[0]) => {
    const first = shell.keymap.chords(action)[0];
    return first ? displayChord(first, shell.platform) : "";
  };

  return (
    <main className="flex h-screen flex-col bg-background text-foreground">
      <TabBar
        workspace={workspace}
        tabs={tabs}
        activeTabId={activeTabId}
        newTabHint={`New tab (${chord("tab.new")})`}
      />
      <div className="min-h-0 flex-1">
        {active ? (
          <PaneTree key={active.id} workspace={workspace} tab={active} />
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-3 text-sm opacity-80">
            <p>No open terminals.</p>
            <div className="flex gap-2">
              <button
                type="button"
                className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
                onClick={() => workspace.newTab()}
              >
                New tab <kbd className="ml-2 text-xs opacity-60">{chord("tab.new")}</kbd>
              </button>
              <button
                type="button"
                className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
                onClick={() => shell.setConnectOpen(true)}
              >
                Connect to host{" "}
                <kbd className="ml-2 text-xs opacity-60">{chord("ssh.connect")}</kbd>
              </button>
            </div>
            <p className="opacity-60">
              All commands: <kbd>{chord("palette.open")}</kbd>
            </p>
          </div>
        )}
      </div>
      {/* Mounted up front so the first keystroke after opening reaches its input. */}
      <CommandPalette shell={shell} />
      <ConnectDialog shell={shell} />
    </main>
  );
}
