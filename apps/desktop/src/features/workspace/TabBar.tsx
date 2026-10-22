// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useState, useSyncExternalStore } from "react";
import type { SessionSpec } from "@/ipc/index.ts";

import type { TerminalController } from "@/features/terminal/controller.ts";

import { paneIds } from "./layout.ts";
import type { Tab, Workspace } from "./workspace.ts";

interface TabBarProps {
  workspace: Workspace<TerminalController, SessionSpec>;
  tabs: readonly Tab[];
  activeTabId: string | undefined;
  newTabHint: string;
}

/** The tab strip. Tabs reorder by drag and drop, or by the move-tab shortcuts. */
export function TabBar({ workspace, tabs, activeTabId, newTabHint }: TabBarProps) {
  const [dragged, setDragged] = useState<string | undefined>();

  return (
    <div className="flex h-9 shrink-0 items-stretch border-b border-current/10 text-sm select-none">
      <div
        role="tablist"
        aria-label="Terminal tabs"
        className="flex min-w-0 flex-1 items-stretch overflow-x-auto"
      >
        {tabs.map((tab, index) => (
          <TabButton
            key={tab.id}
            workspace={workspace}
            tab={tab}
            active={tab.id === activeTabId}
            onDragStart={() => setDragged(tab.id)}
            onDragEnd={() => setDragged(undefined)}
            onDrop={() => {
              if (dragged) workspace.moveTab(dragged, index);
              setDragged(undefined);
            }}
          />
        ))}
      </div>
      <button
        type="button"
        aria-label="New tab"
        title={newTabHint}
        className="px-3 text-base opacity-70 hover:opacity-100"
        onClick={() => workspace.newTab()}
      >
        +
      </button>
    </div>
  );
}

function TabButton({
  workspace,
  tab,
  active,
  onDragStart,
  onDragEnd,
  onDrop,
}: {
  workspace: Workspace<TerminalController, SessionSpec>;
  tab: Tab;
  active: boolean;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDrop: () => void;
}) {
  const controller = workspace.terminal(tab.focused);
  const title = useSyncExternalStore(
    (listener) => controller?.subscribe(listener) ?? (() => {}),
    () => controller?.title ?? "",
  );
  const label = title || controller?.defaultTitle || "Terminal";
  // A tab is marked when any of its panes is guarded, not just the focused one.
  const subscribeGuards = useCallback(
    (listener: () => void) => {
      const stops = paneIds(tab.root).map(
        (id) => workspace.terminal(id)?.subscribeGuard(listener) ?? (() => {}),
      );
      return () => {
        for (const stop of stops) stop();
      };
    },
    [workspace, tab.root],
  );
  const guarded = useSyncExternalStore(subscribeGuards, () =>
    paneIds(tab.root).some((id) => workspace.terminal(id)?.guarded ?? false),
  );

  return (
    <div
      role="tab"
      aria-selected={active}
      tabIndex={active ? 0 : -1}
      draggable
      data-tab-id={tab.id}
      data-guarded={guarded}
      className={`group flex max-w-56 min-w-28 items-center gap-2 border-r border-current/10 px-3 ${
        active ? "bg-current/[0.06]" : "opacity-70 hover:opacity-100"
      } ${guarded ? "border-b-2 border-b-[var(--prod)]" : ""}`}
      onClick={() => workspace.activateTab(tab.id)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") workspace.activateTab(tab.id);
      }}
      onDragStart={(event) => {
        event.dataTransfer.effectAllowed = "move";
        onDragStart();
      }}
      onDragEnd={onDragEnd}
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        event.preventDefault();
        onDrop();
      }}
    >
      {guarded && (
        <span className="rounded bg-[var(--prod)] px-1 text-[10px] leading-4 font-semibold text-white">
          PROD
        </span>
      )}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <button
        type="button"
        aria-label={`Close ${label}`}
        className="rounded px-1 opacity-0 group-hover:opacity-70 hover:!opacity-100 focus-visible:opacity-100"
        onClick={(event) => {
          event.stopPropagation();
          workspace.closeTab(tab.id);
        }}
      >
        ×
      </button>
    </div>
  );
}
