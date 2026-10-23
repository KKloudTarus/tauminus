// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useSyncExternalStore } from "react";
import { Group, type Layout, Panel, Separator } from "react-resizable-panels";
import type { SessionSpec } from "@/ipc/index.ts";

import type { TerminalController } from "@/features/terminal/controller.ts";
import { TerminalView } from "@/features/terminal/TerminalView.tsx";

import type { LayoutNode, PaneId } from "./layout.ts";
import type { Broadcast } from "./broadcast.ts";
import type { Tab, Workspace } from "./workspace.ts";

interface PaneTreeProps {
  workspace: Workspace<TerminalController, SessionSpec>;
  tab: Tab;
  /** The panes that type together; without it none are marked. */
  broadcast?: Broadcast | undefined;
  onUnlockVault?: ((retry: () => void) => void) | undefined;
}

/** Renders one tab's layout: nested resizable groups with a terminal in each leaf. */
export function PaneTree({ workspace, tab, broadcast, onUnlockVault }: PaneTreeProps) {
  return (
    <Node
      workspace={workspace}
      tab={tab}
      node={tab.root}
      broadcast={broadcast}
      onUnlockVault={onUnlockVault}
    />
  );
}

function Node({
  workspace,
  tab,
  node,
  broadcast,
  onUnlockVault,
}: PaneTreeProps & { node: LayoutNode }) {
  if (node.kind === "pane") {
    return (
      <TerminalPane
        workspace={workspace}
        paneId={node.id}
        focused={tab.focused === node.id}
        split={tab.root.kind !== "pane"}
        broadcast={broadcast}
        onUnlockVault={onUnlockVault}
      />
    );
  }
  const onLayoutChange = (layout: Layout) => {
    const sizes = node.children.map((child) => layout[child.id] ?? 0);
    workspace.resize(tab.id, node.id, sizes);
  };
  return (
    <Group
      // A new child set remounts the group so it picks up the tree's sizes.
      key={node.children.map((child) => child.id).join(",")}
      id={node.id}
      orientation={node.direction === "row" ? "horizontal" : "vertical"}
      onLayoutChange={onLayoutChange}
      className="h-full w-full"
    >
      {node.children.map((child, index) => (
        <ChildPanel key={child.id} index={index}>
          <Panel id={child.id} defaultSize={`${node.sizes[index] ?? 0}%`} minSize="8%">
            <Node
              workspace={workspace}
              tab={tab}
              node={child}
              broadcast={broadcast}
              onUnlockVault={onUnlockVault}
            />
          </Panel>
        </ChildPanel>
      ))}
    </Group>
  );
}

/** A separator before every panel but the first. Fragments keep panels direct children of the group. */
function ChildPanel({ index, children }: { index: number; children: React.ReactNode }) {
  return (
    <>
      {index > 0 && (
        <Separator className="w-px bg-line-strong transition-colors hover:bg-accent data-[orientation=horizontal]:h-px data-[orientation=horizontal]:w-auto data-[separator=active]:bg-accent" />
      )}
      {children}
    </>
  );
}

function TerminalPane({
  workspace,
  paneId,
  focused,
  split,
  broadcast,
  onUnlockVault,
}: {
  workspace: Workspace<TerminalController, SessionSpec>;
  paneId: PaneId;
  focused: boolean;
  /** Whether the tab holds more than this pane; only then is focus marked. */
  split: boolean;
  broadcast: Broadcast | undefined;
  onUnlockVault: ((retry: () => void) => void) | undefined;
}) {
  const controller = workspace.terminal(paneId);
  const title = useSyncExternalStore(
    (listener) => controller?.subscribe(listener) ?? (() => {}),
    () => controller?.title ?? "",
  );

  const guarded = useSyncExternalStore(
    (listener) => controller?.subscribeGuard(listener) ?? (() => {}),
    () => controller?.guarded ?? false,
  );

  const broadcasting = useSyncExternalStore(
    broadcast?.subscribe ?? noSubscribe,
    () => broadcast?.has(paneId) ?? false,
  );

  useEffect(() => {
    if (focused) controller?.focus();
  }, [focused, controller]);

  if (!controller) {
    return null;
  }
  return (
    <section
      aria-label={title || "Terminal"}
      data-pane-id={paneId}
      data-focused={focused}
      data-guarded={guarded}
      data-broadcast={broadcasting}
      className={`relative flex h-full min-h-0 min-w-0 flex-col bg-terminal ${
        guarded
          ? `ring-inset ring-prod ${focused ? "ring-2" : "ring-1"}`
          : broadcasting
            ? `ring-inset ring-warn ${focused ? "ring-2" : "ring-1"}`
            : focused && split
              ? "ring-1 ring-accent/60 ring-inset"
              : ""
      }`}
      onPointerDown={() => workspace.focusPane(paneId)}
      onFocusCapture={() => {
        if (!focused) workspace.focusPane(paneId);
      }}
    >
      {broadcasting && (
        <span
          data-testid="broadcast-badge"
          className="pointer-events-none absolute top-2 right-3 z-10 rounded-full bg-warn px-2 py-0.5 text-[11px] font-semibold text-black"
        >
          Broadcast
        </span>
      )}
      <TerminalView
        controller={controller}
        className="min-h-0 flex-1"
        {...(onUnlockVault ? { onUnlockVault } : {})}
      />
    </section>
  );
}

const noSubscribe = (): (() => void) => () => {};
