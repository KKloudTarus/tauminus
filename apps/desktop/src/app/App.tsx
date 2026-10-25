// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useEffect, useMemo, useRef, useSyncExternalStore } from "react";

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
import { AiDialog } from "@/features/ai/AiDialog.tsx";
import { VaultDialog } from "@/features/vault/VaultDialog.tsx";
import { SyncDialog } from "@/features/sync/SyncDialog.tsx";
import { PaneTree } from "@/features/workspace/PaneTree.tsx";
import { TabBar } from "@/features/workspace/TabBar.tsx";

import { CommandPalette } from "./CommandPalette.tsx";
import { Icon } from "./icons.tsx";
import { NARROW, useMediaQuery } from "./media.ts";
import type { HostView } from "@/ipc/index.ts";

import type { AppShell } from "./shell.ts";

export function App({ shell }: { shell: AppShell }) {
  const { workspace } = shell;
  const { tabs, activeTabId } = useSyncExternalStore(workspace.subscribe, workspace.getSnapshot);
  const active = tabs.find((tab) => tab.id === activeTabId);
  const {
    sidebarOpen,
    vaultOpen,
    syncOpen,
    historyOpen,
    forwardsOpen,
    snippetsOpen,
    workspacesOpen,
    serialOpen,
    recordingsOpen,
    triggersOpen,
    capturesOpen,
    assistant,
    snippetToFill,
    snippetHosts,
    files,
    theme,
  } = useSyncExternalStore(shell.subscribe, shell.getState);
  const narrow = useMediaQuery(NARROW);
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
  // On a phone the host list covers the terminal, so connecting puts it away.
  const connectHost = useCallback(
    (host: HostView) => {
      shell.connectHost(host);
      if (narrow) shell.setSidebarOpen(false, LAYOUT_ONLY);
    },
    [shell, narrow],
  );

  // A narrow window starts on the terminal: the host list closes on the way
  // in and comes back on the way out if it was open before.
  const closedForNarrow = useRef(false);
  useEffect(() => {
    if (narrow && shell.getState().sidebarOpen) {
      closedForNarrow.current = true;
      shell.setSidebarOpen(false, LAYOUT_ONLY);
    } else if (!narrow && closedForNarrow.current) {
      closedForNarrow.current = false;
      shell.setSidebarOpen(true, LAYOUT_ONLY);
    }
  }, [narrow, shell]);
  const openVault = useCallback(() => shell.setVaultOpen(true), [shell]);
  const openSync = useCallback(() => shell.setSyncOpen(true), [shell]);
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

  // The drawer takes the keyboard while it is open, so keys never reach a
  // terminal it covers; closing it hands the keyboard back to that terminal.
  const drawerOpen = narrow && sidebarOpen;
  const drawerRef = useRef<HTMLDivElement>(null);
  const wasDrawerOpen = useRef(false);
  useEffect(() => {
    if (drawerOpen) {
      drawerRef.current?.querySelector<HTMLInputElement>('input[type="search"]')?.focus();
    } else if (wasDrawerOpen.current) {
      const now = workspace.getSnapshot();
      const tab = now.tabs.find((candidate) => candidate.id === now.activeTabId);
      if (tab) workspace.terminal(tab.focused)?.focus();
    }
    wasDrawerOpen.current = drawerOpen;
  }, [drawerOpen, workspace]);

  // Escape puts the phone drawer away, unless a dialog opened from it (the
  // host editor, the palette) takes the key for itself.
  useEffect(() => {
    if (!narrow || !sidebarOpen) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      const inDialog =
        event.target instanceof Element && event.target.closest('dialog, [role="dialog"]');
      if (event.key === "Escape" && !event.defaultPrevented && !inDialog) {
        shell.setSidebarOpen(false, LAYOUT_ONLY);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [narrow, sidebarOpen, shell]);

  // Shortcuts that arrive while focus is outside a terminal, such as on the tab bar.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const state = shell.getState();
      const modal =
        state.paletteOpen ||
        state.connectOpen ||
        state.vaultOpen ||
        state.syncOpen ||
        state.historyOpen ||
        state.forwardsOpen ||
        state.snippetsOpen ||
        state.workspacesOpen ||
        state.serialOpen ||
        state.recordingsOpen ||
        state.triggersOpen ||
        state.capturesOpen ||
        state.assistant !== undefined ||
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
    <main className="flex h-dvh flex-col bg-background pt-[env(safe-area-inset-top)] text-foreground">
      <div inert={drawerOpen} className="contents">
        <TabBar
          workspace={workspace}
          tabs={tabs}
          activeTabId={activeTabId}
          newTabHint={`New tab (${chord("tab.new")})`}
          start={
            <button
              type="button"
              aria-label="Hosts"
              aria-pressed={sidebarOpen}
              title={`Hosts (${chord("hosts.toggle")})`}
              className="icon-btn"
              // The drawer on a phone opens and closes without changing the saved choice.
              onClick={() => shell.setSidebarOpen(!sidebarOpen, narrow ? LAYOUT_ONLY : {})}
            >
              <Icon name="sidebar" />
            </button>
          }
          end={
            <>
              <button
                type="button"
                aria-label={`Theme: ${theme}`}
                title={`Theme: ${theme}. Click for the next one.`}
                className="icon-btn"
                onClick={() => shell.run("theme.cycle")}
              >
                <Icon name={theme === "light" ? "sun" : theme === "dark" ? "moon" : "monitor"} />
              </button>
              <button
                type="button"
                aria-label="All commands"
                title={`All commands (${chord("palette.open")})`}
                className="icon-btn"
                onClick={() => shell.setPaletteOpen(true)}
              >
                <Icon name="command" />
              </button>
            </>
          }
        />
      </div>
      <div className="relative flex min-h-0 flex-1">
        {sidebarOpen && !narrow && (
          <Sidebar
            store={shell.hosts}
            onConnect={connectHost}
            onOpenVault={openVault}
            {...(shell.sync ? { onOpenSync: openSync } : {})}
            {...(shell.files ? { onFiles: openFiles } : {})}
            identities={vault.identities}
            snippets={snippetList}
            highlightSets={highlightSets}
          />
        )}
        {sidebarOpen && narrow && (
          // A drawer over the terminal; the scrim and Escape close it.
          <div ref={drawerRef} className="fixed inset-0 z-30 flex">
            <Sidebar
              store={shell.hosts}
              onConnect={connectHost}
              onOpenVault={openVault}
              {...(shell.sync ? { onOpenSync: openSync } : {})}
              {...(shell.files ? { onFiles: openFiles } : {})}
              identities={vault.identities}
              snippets={snippetList}
              className="drawer w-[min(340px,calc(100vw-48px))] border-r pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)] shadow-lg"
            />
            <button
              type="button"
              aria-label="Close hosts"
              className="min-w-0 flex-1 bg-[var(--scrim)]"
              onClick={() => shell.setSidebarOpen(false, LAYOUT_ONLY)}
            />
          </div>
        )}
        <div inert={drawerOpen} className="min-w-0 flex-1 bg-terminal">
          {active ? (
            <PaneTree
              key={active.id}
              workspace={workspace}
              broadcast={shell.broadcast}
              tab={active}
              onUnlockVault={(retry) => shell.unlockVaultThen(retry)}
              {...(shell.sync
                ? {
                    onReview: (retry: () => void, host: string | undefined) =>
                      shell.reviewSyncThen(retry, host),
                  }
                : {})}
            />
          ) : (
            <EmptyState
              newTab={() => workspace.newTab()}
              connect={() => shell.setConnectOpen(true)}
              openPalette={() => shell.setPaletteOpen(true)}
              chord={chord}
            />
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
      {assistant && shell.ai && (
        <AiDialog
          client={shell.ai}
          mode={assistant.mode}
          session={assistant.session}
          onClose={() => shell.closeAssistant()}
          onInsert={(command) => shell.insertSuggestion(assistant.pane, command)}
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
      {syncOpen && shell.sync && (
        <SyncDialog
          store={shell.sync}
          onClose={() => shell.setSyncOpen(false)}
          onOpenVault={() => {
            shell.setSyncOpen(false);
            shell.setVaultOpen(true);
          }}
          onReviewed={(hosts) => shell.syncReviewed(hosts)}
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

/** A sidebar change the phone layout makes; the desktop choice stays saved. */
const LAYOUT_ONLY = { persist: false } as const;

/** What the window shows with every tab closed: the ways to start. */
function EmptyState({
  newTab,
  connect,
  openPalette,
  chord,
}: {
  newTab: () => void;
  connect: () => void;
  openPalette: () => void;
  chord: (action: "tab.new" | "ssh.connect" | "palette.open") => string;
}) {
  const rows = [
    { label: "New tab", icon: "terminal", hint: chord("tab.new"), run: newTab },
    { label: "Connect to host", icon: "plug", hint: chord("ssh.connect"), run: connect },
    { label: "All commands", icon: "command", hint: chord("palette.open"), run: openPalette },
  ] as const;
  return (
    <div className="flex h-full flex-col items-center justify-center p-6">
      <div className="w-full max-w-sm">
        <div className="mb-5 flex items-center gap-3">
          <span className="flex size-10 items-center justify-center rounded-xl border border-line-strong bg-surface-2 font-mono text-lg font-semibold text-accent">
            τ
          </span>
          <div>
            <p className="font-semibold">No open terminals.</p>
            <p className="text-[13px] text-muted">Open a shell here or on a saved host.</p>
          </div>
        </div>
        <div className="flex flex-col gap-1">
          {rows.map((row) => (
            <button
              key={row.label}
              type="button"
              onClick={row.run}
              className="group flex h-11 items-center gap-3 rounded-lg border border-line bg-surface-1 px-3 text-left text-[13px] font-medium transition-colors hover:border-line-strong hover:bg-surface-2"
            >
              <Icon name={row.icon} className="size-4 text-muted group-hover:text-accent" />
              <span className="flex-1">{row.label}</span>
              {row.hint && <kbd className="kbd">{row.hint}</kbd>}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
