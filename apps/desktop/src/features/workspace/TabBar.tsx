// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useState, useSyncExternalStore } from "react";
import type { SessionSpec } from "@/ipc/index.ts";

import {
  type Activity,
  type TerminalController,
  mostUrgent,
} from "@/features/terminal/controller.ts";

import { Icon } from "@/app/icons.tsx";

import { paneIds } from "./layout.ts";
import type { Tab, Workspace } from "./workspace.ts";

interface TabBarProps {
  workspace: Workspace<TerminalController, SessionSpec>;
  tabs: readonly Tab[];
  activeTabId: string | undefined;
  newTabHint: string;
  /** Leading and trailing controls, such as the hosts toggle and the palette. */
  start?: React.ReactNode;
  end?: React.ReactNode;
}

/** The tab strip. Tabs reorder by drag and drop, or by the move-tab shortcuts. */
export function TabBar({ workspace, tabs, activeTabId, newTabHint, start, end }: TabBarProps) {
  const [dragged, setDragged] = useState<string | undefined>();

  return (
    <div className="flex h-10 shrink-0 items-stretch gap-1 border-b border-line bg-background pr-1.5 pl-1.5 text-[13px] select-none">
      {start && <div className="flex items-center gap-0.5">{start}</div>}
      {/* Scrolls the tabs and the new-tab button together; the tablist holds only tabs. */}
      <div className="flex min-w-0 flex-1 items-end gap-0.5 overflow-x-auto pt-1.5 [scrollbar-width:none]">
        <div role="tablist" aria-label="Terminal tabs" className="flex h-full items-end gap-0.5">
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
          className="icon-btn mb-1 self-end"
          onClick={() => workspace.newTab()}
        >
          <Icon name="plus" />
        </button>
      </div>
      {end && <div className="flex items-center gap-0.5">{end}</div>}
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
  // The most urgent thing that happened in any of its panes while hidden.
  const subscribePanes = useCallback(
    (listener: () => void) => {
      const stops = paneIds(tab.root).map(
        (id) => workspace.terminal(id)?.subscribe(listener) ?? (() => {}),
      );
      return () => {
        for (const stop of stops) stop();
      };
    },
    [workspace, tab.root],
  );
  const activity = useSyncExternalStore(subscribePanes, () =>
    mostUrgent(paneIds(tab.root).map((id) => workspace.terminal(id)?.getActivity() ?? "idle")),
  );

  return (
    <div
      role="tab"
      aria-selected={active}
      tabIndex={active ? 0 : -1}
      draggable
      data-tab-id={tab.id}
      data-guarded={guarded}
      data-activity={activity}
      title={label}
      className={`group relative flex h-full max-w-60 min-w-32 cursor-default items-center gap-2 rounded-t-lg px-3 transition-colors ${
        active
          ? "bg-terminal text-foreground shadow-[inset_0_1px_0_var(--line-strong),inset_1px_0_0_var(--line),inset_-1px_0_0_var(--line)]"
          : "text-muted hover:bg-surface-2/60 hover:text-foreground"
      }`}
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
        <>
          <span
            aria-hidden
            className="absolute inset-x-0 top-0 h-[3px] rounded-t-lg bg-prod-fill"
          />
          <span className="chip chip-prod shrink-0">PROD</span>
        </>
      )}
      {!guarded && (
        <Icon
          name="terminal"
          className={`size-3.5 shrink-0 ${active ? "text-accent" : "text-faint"}`}
        />
      )}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <ActivityDot activity={activity} />
      <button
        type="button"
        aria-label={`Close ${label}`}
        className={`-mr-1.5 flex size-5 shrink-0 pointer-coarse:size-8 items-center justify-center rounded text-faint hover:bg-surface-2 hover:text-foreground focus-visible:opacity-100 ${
          active ? "opacity-100" : "opacity-0 group-hover:opacity-100"
        }`}
        onClick={(event) => {
          event.stopPropagation();
          workspace.closeTab(tab.id);
        }}
      >
        <Icon name="x" className="size-3.5" />
      </button>
    </div>
  );
}

const DOTS: Record<Exclude<Activity, "idle">, { label: string; className: string }> = {
  unread: { label: "New output", className: "bg-accent" },
  waiting: { label: "Waiting for input", className: "bg-warn" },
  error: { label: "Failed", className: "bg-danger" },
};

/** One dot for what happened in a tab while it was hidden. */
function ActivityDot({ activity }: { activity: Activity }) {
  if (activity === "idle") return null;
  const dot = DOTS[activity];
  return (
    <span title={dot.label} data-testid="activity-dot" className="flex shrink-0 items-center">
      <span aria-hidden className={`size-1.5 rounded-full ${dot.className}`} />
      <span className="sr-only">{dot.label}</span>
    </span>
  );
}
