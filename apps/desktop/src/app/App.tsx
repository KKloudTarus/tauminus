// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";

import { displayChord } from "@/features/keymap/keymap.ts";
import { Sidebar } from "@/features/hosts/Sidebar.tsx";
import { ConnectDialog } from "@/features/ssh/ConnectDialog.tsx";
import { HistoryDialog } from "@/features/history/HistoryDialog.tsx";
import { VaultDialog } from "@/features/vault/VaultDialog.tsx";
import { PaneTree } from "@/features/workspace/PaneTree.tsx";
import { TabBar } from "@/features/workspace/TabBar.tsx";

import { CommandPalette } from "./CommandPalette.tsx";
import type { HostView } from "@/ipc/index.ts";

import type { AppShell } from "./shell.ts";

export function App({ shell }: { shell: AppShell }) {
  const { workspace } = shell;
  const { tabs, activeTabId } = useSyncExternalStore(workspace.subscribe, workspace.getSnapshot);
  const active = tabs.find((tab) => tab.id === activeTabId);
  const { sidebarOpen, vaultOpen, historyOpen } = useSyncExternalStore(
    shell.subscribe,
    shell.getState,
  );
  const vault = useSyncExternalStore(shell.vault.subscribe, shell.vault.getSnapshot);
  const savedHosts = useSyncExternalStore(shell.hosts.subscribe, shell.hosts.getSnapshot).view
    .hosts;
  const keyTools = useMemo(
    () => ({ hosts: savedHosts, copy: (text: string) => shell.copyText(text) }),
    [savedHosts, shell],
  );
  // Stable, so a vault change does not rebuild the host tree. O(hosts) each.
  const connectHost = useCallback((host: HostView) => shell.connectHost(host), [shell]);
  const openVault = useCallback(() => shell.setVaultOpen(true), [shell]);

  // Typing and clicking keep an open vault from locking. Capture phase: the
  // terminal handles its keys before they would bubble here.
  useEffect(() => {
    const onActivity = () => shell.vault.activity();
    // The vault may have locked itself while the window was in the background.
    const onFocus = () => {
      shell.vault.refresh().catch((error: unknown) => console.warn("vault status failed", error));
    };
    onFocus();
    window.addEventListener("keydown", onActivity, true);
    window.addEventListener("pointerdown", onActivity, true);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("keydown", onActivity, true);
      window.removeEventListener("pointerdown", onActivity, true);
      window.removeEventListener("focus", onFocus);
    };
  }, [shell]);

  // Shortcuts that arrive while focus is outside a terminal, such as on the tab bar.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const state = shell.getState();
      const modal = state.paletteOpen || state.connectOpen || state.vaultOpen || state.historyOpen;
      if (!event.defaultPrevented && !modal) {
        shell.handleKey(event);
      }
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
      <div className="flex min-h-0 flex-1">
        {sidebarOpen && (
          <Sidebar
            store={shell.hosts}
            onConnect={connectHost}
            onOpenVault={openVault}
            identities={vault.identities}
          />
        )}
        <div className="min-w-0 flex-1">
          {active ? (
            <PaneTree
              key={active.id}
              workspace={workspace}
              tab={active}
              onUnlockVault={(retry) => shell.unlockVaultThen(retry)}
            />
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
      </div>
      {/* Mounted up front so the first keystroke after opening reaches its input. */}
      <CommandPalette shell={shell} />
      <ConnectDialog shell={shell} />
      {historyOpen && shell.history && (
        <HistoryDialog
          client={shell.history}
          hosts={savedHosts}
          onClose={() => shell.setHistoryOpen(false)}
          onInsert={(command) => shell.insertText(command)}
          onCopy={(text) => shell.copyText(text)}
          onUnlock={(retry) => shell.unlockVaultThen(retry)}
        />
      )}
      {vaultOpen && (
        <VaultDialog
          store={shell.vault}
          tools={keyTools}
          onClose={() => shell.setVaultOpen(false)}
          onUnlocked={() => shell.vaultUnlocked()}
        />
      )}
    </main>
  );
}
