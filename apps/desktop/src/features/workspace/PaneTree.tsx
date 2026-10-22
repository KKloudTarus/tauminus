// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useSyncExternalStore } from "react";
import { Group, type Layout, Panel, Separator } from "react-resizable-panels";
import type { SessionSpec } from "@/ipc/index.ts";

import type { TerminalController } from "@/features/terminal/controller.ts";
import { TerminalView } from "@/features/terminal/TerminalView.tsx";

import type { LayoutNode, PaneId } from "./layout.ts";
import type { Tab, Workspace } from "./workspace.ts";

interface PaneTreeProps {
  workspace: Workspace<TerminalController, SessionSpec>;
  tab: Tab;
  onUnlockVault?: ((retry: () => void) => void) | undefined;
}

/** Renders one tab's layout: nested resizable groups with a terminal in each leaf. */
export function PaneTree({ workspace, tab, onUnlockVault }: PaneTreeProps) {
  return <Node workspace={workspace} tab={tab} node={tab.root} onUnlockVault={onUnlockVault} />;
}

function Node({ workspace, tab, node, onUnlockVault }: PaneTreeProps & { node: LayoutNode }) {
  if (node.kind === "pane") {
    return (
      <TerminalPane
        workspace={workspace}
        paneId={node.id}
        focused={tab.focused === node.id}
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
            <Node workspace={workspace} tab={tab} node={child} onUnlockVault={onUnlockVault} />
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
        <Separator className="w-px bg-current/15 transition-colors hover:bg-current/40 data-[orientation=horizontal]:h-px data-[orientation=horizontal]:w-auto" />
      )}
      {children}
    </>
  );
}

function TerminalPane({
  workspace,
  paneId,
  focused,
  onUnlockVault,
}: {
  workspace: Workspace<TerminalController, SessionSpec>;
  paneId: PaneId;
  focused: boolean;
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
      className={`flex h-full min-h-0 min-w-0 flex-col ${
        guarded
          ? `ring-inset ring-[var(--prod)] ${focused ? "ring-2" : "ring-1"}`
          : focused
            ? "ring-1 ring-inset ring-[var(--accent)]"
            : ""
      }`}
      onPointerDown={() => workspace.focusPane(paneId)}
      onFocusCapture={() => {
        if (!focused) workspace.focusPane(paneId);
      }}
    >
      <TerminalView
        controller={controller}
        className="min-h-0 flex-1"
        {...(onUnlockVault ? { onUnlockVault } : {})}
      />
    </section>
  );
}
