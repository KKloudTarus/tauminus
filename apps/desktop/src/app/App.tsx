// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";

import { displayChord } from "@/features/keymap/keymap.ts";
import { Sidebar } from "@/features/hosts/Sidebar.tsx";
import { ConnectDialog } from "@/features/ssh/ConnectDialog.tsx";
import { FilesView } from "@/features/files/FilesView.tsx";
import { ForwardsDialog } from "@/features/forwards/ForwardsDialog.tsx";
import { RecordingsDialog } from "@/features/recording/RecordingsDialog.tsx";
import { TriggersDialog } from "@/features/triggers/TriggersDialog.tsx";
import { CapturesDialog } from "@/features/triggers/CapturesDialog.tsx";
import { SerialDialog } from "@/features/serial/SerialDialog.tsx";
import { SnippetForm } from "@/features/snippets/SnippetForm.tsx";
import { SnippetHosts } from "@/features/snippets/SnippetHosts.tsx";
import { WorkspacesDialog } from "@/features/workspace/WorkspacesDialog.tsx";
import { SnippetsDialog } from "@/features/snippets/SnippetsDialog.tsx";
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
  const {
    sidebarOpen,
    vaultOpen,
    historyOpen,
    forwardsOpen,
    snippetsOpen,
    workspacesOpen,
    serialOpen,
    recordingsOpen,
    triggersOpen,
    capturesOpen,
    snippetToFill,
    snippetHosts,
    files,
  } = useSyncExternalStore(shell.subscribe, shell.getState);
  const vault = useSyncExternalStore(shell.vault.subscribe, shell.vault.getSnapshot);
  const snippetList = useSyncExternalStore(
    shell.snippets?.subscribe ?? noSubscribe,
    // Only a loaded library: an empty one would show every startup snippet as deleted.
    () => {
      const state = shell.snippets?.getSnapshot();
      return state?.loaded ? state.snippets : undefined;
    },
  );
  const highlightSets = useSyncExternalStore(
    shell.triggers?.subscribe ?? noSubscribe,
    // Only a loaded library: an empty one would show every set as deleted.
    () => {
      const state = shell.triggers?.getSnapshot();
      return state?.loaded ? state.sets : undefined;
    },
  );
  const savedHosts = useSyncExternalStore(shell.hosts.subscribe, shell.hosts.getSnapshot).view
    .hosts;
  const keyTools = useMemo(
    () => ({ hosts: savedHosts, copy: (text: string) => shell.copyText(text) }),
    [savedHosts, shell],
  );
  // Stable, so a vault change does not rebuild the host tree. O(hosts) each.
  const connectHost = useCallback((host: HostView) => shell.connectHost(host), [shell]);
  const openVault = useCallback(() => shell.setVaultOpen(true), [shell]);
  const openFiles = useCallback((host: HostView) => shell.openFiles(host), [shell]);
  const chooseSnippet = useCallback((id: string) => shell.chooseSnippet(id), [shell]);
  const chooseSnippetHosts = useCallback((id: string) => shell.chooseSnippetHosts(id), [shell]);

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
      const modal =
        state.paletteOpen ||
        state.connectOpen ||
        state.vaultOpen ||
        state.historyOpen ||
        state.forwardsOpen ||
        state.snippetsOpen ||
        state.workspacesOpen ||
        state.serialOpen ||
        state.recordingsOpen ||
        state.triggersOpen ||
        state.capturesOpen ||
        state.snippetToFill !== undefined ||
        state.snippetHosts !== undefined ||
        state.files !== undefined;
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
            {...(shell.files ? { onFiles: openFiles } : {})}
            identities={vault.identities}
            snippets={snippetList}
            highlightSets={highlightSets}
          />
        )}
        <div className="min-w-0 flex-1">
          {active ? (
            <PaneTree
              key={active.id}
              workspace={workspace}
              broadcast={shell.broadcast}
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
      {files && shell.files && (
        <FilesView
          store={files.store}
          client={shell.files}
          hostLabel={files.host.label}
          onClose={() => shell.closeFiles()}
        />
      )}
      {forwardsOpen && shell.forwards && (
        <ForwardsDialog
          store={shell.forwards}
          hosts={savedHosts}
          onClose={() => shell.setForwardsOpen(false)}
        />
      )}
      {snippetsOpen && shell.snippets && (
        <SnippetsDialog
          store={shell.snippets}
          onRun={chooseSnippet}
          onRunOnHosts={chooseSnippetHosts}
          onClose={() => shell.setSnippetsOpen(false)}
        />
      )}
      {serialOpen && shell.serial && (
        <SerialDialog
          client={shell.serial}
          onOpen={(port) => shell.openSerial(port)}
          onClose={() => shell.setSerialOpen(false)}
        />
      )}
      {recordingsOpen && shell.recordings && (
        <RecordingsDialog
          client={shell.recordings}
          onClose={() => shell.setRecordingsOpen(false)}
          onUnlock={(retry) => shell.unlockVaultThen(retry)}
        />
      )}
      {triggersOpen && shell.triggers && (
        <TriggersDialog store={shell.triggers} onClose={() => shell.setTriggersOpen(false)} />
      )}
      {capturesOpen && (
        <CapturesDialog
          store={shell.captures}
          onCopy={(text) => shell.copyText(text)}
          onClose={() => shell.setCapturesOpen(false)}
        />
      )}
      {workspacesOpen && (
        <WorkspacesDialog
          workspaces={shell.workspaces}
          onSave={(name) => shell.saveWorkspace(name)}
          onOpen={(id) => void shell.openWorkspace(id)}
          onClose={() => shell.setWorkspacesOpen(false)}
        />
      )}
      {snippetHosts && (
        <SnippetHosts
          snippet={snippetHosts}
          hosts={savedHosts}
          onRun={(hosts) => shell.chooseSnippet(snippetHosts.id, hosts)}
          onClose={() => shell.closeSnippetHosts()}
        />
      )}
      {snippetToFill && (
        <SnippetForm
          snippet={snippetToFill.snippet}
          onRun={(values) =>
            void shell.runSnippet(snippetToFill.snippet.id, values, snippetToFill.hosts)
          }
          onClose={() => shell.closeSnippetForm()}
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

const noSubscribe = (): (() => void) => () => {};
