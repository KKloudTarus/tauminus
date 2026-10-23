// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Command, useCommandState } from "cmdk";
import { useMemo, useSyncExternalStore } from "react";

import type { ForwardsStore } from "@/features/forwards/forwards-store.ts";
import { matchRules, ruleChoices } from "@/features/forwards/rules.ts";
import { ACTIONS, type Action, displayChord } from "@/features/keymap/keymap.ts";
import { type SnippetsStore, snippetTitle } from "@/features/snippets/snippets-store.ts";
import type { HostView } from "@/ipc/index.ts";

import { Icon } from "./icons.tsx";
import { ACTION_LABELS, type AppShell } from "./shell.ts";

const ITEM =
  "flex h-9 cursor-default items-center gap-3 rounded-lg px-3 text-[13px] text-muted data-[selected=true]:bg-accent-soft data-[selected=true]:text-foreground";

/** Every action, searchable by name, with its shortcut, then matching forwarding rules. */
export function CommandPalette({ shell }: { shell: AppShell }) {
  const { paletteOpen } = useSyncExternalStore(shell.subscribe, shell.getState);
  const choices = ACTIONS.filter((action) => action !== "palette.open");

  const run = (action: Action) => {
    shell.setPaletteOpen(false);
    shell.run(action);
  };

  return (
    <Command.Dialog
      open={paletteOpen}
      onOpenChange={(open) => shell.setPaletteOpen(open)}
      label="Command palette"
      overlayClassName="fixed inset-0 z-40 bg-[var(--scrim)]"
      contentClassName="panel palette fixed top-[12vh] left-1/2 z-50 flex max-h-[min(520px,76dvh)] w-[min(620px,calc(100vw-32px))] -translate-x-1/2 flex-col overflow-hidden"
    >
      <div className="flex items-center gap-3 border-b border-line px-4">
        <Icon name="search" className="size-4 shrink-0 text-faint" />
        <Command.Input
          placeholder="Type a command"
          className="h-12 min-w-0 flex-1 bg-transparent text-[15px] outline-none placeholder:text-faint focus-visible:outline-none"
        />
      </div>
      <Command.List className="min-h-0 flex-1 overflow-y-auto p-1.5 [scroll-padding-block:6px]">
        <Command.Empty className="px-3 py-8 text-center text-[13px] text-faint">
          No matching command
        </Command.Empty>
        {choices.map((action) => {
          const chord = shell.keymap.chords(action)[0];
          return (
            <Command.Item
              key={action}
              value={ACTION_LABELS[action]}
              onSelect={() => run(action)}
              className={ITEM}
            >
              <span className="flex-1 truncate">{ACTION_LABELS[action]}</span>
              {chord && <kbd className="kbd">{displayChord(chord, shell.platform)}</kbd>}
            </Command.Item>
          );
        })}
        {paletteOpen && <WorkspaceItems shell={shell} />}
        {paletteOpen && shell.snippets && <SnippetItems shell={shell} snippets={shell.snippets} />}
        {paletteOpen && shell.forwards && <RuleItems shell={shell} forwards={shell.forwards} />}
      </Command.List>
      <div
        aria-hidden="true"
        className="hidden items-center gap-4 border-t border-line px-4 py-2 text-[11px] text-faint sm:flex"
      >
        <span className="flex items-center gap-1.5">
          <kbd className="kbd">↑</kbd>
          <kbd className="kbd">↓</kbd> to move
        </span>
        <span className="flex items-center gap-1.5">
          <kbd className="kbd">↵</kbd> to run
        </span>
        <span className="flex items-center gap-1.5">
          <kbd className="kbd">Esc</kbd> to close
        </span>
      </div>
    </Command.Dialog>
  );
}

/**
 * Start or stop for the rules that match what was typed, at most
 * {@link RULE_ITEMS}. Mounted only while the palette is open, so a large
 * host list costs nothing otherwise; matching is O(R) per keystroke over
 * names lowercased once per host list.
 */
function RuleItems({ shell, forwards }: { shell: AppShell; forwards: ForwardsStore }) {
  const hosts: readonly HostView[] = useSyncExternalStore(
    shell.hosts.subscribe,
    shell.hosts.getSnapshot,
  ).view.hosts;
  // Read so the verbs follow what runs.
  useSyncExternalStore(forwards.subscribe, forwards.getSnapshot);
  const search = useCommandState((state) => state.search);
  const all = useMemo(() => ruleChoices(hosts), [hosts]);
  return matchRules(all, search, RULE_ITEMS).map(({ hostId, hostLabel, ruleId, ruleLabel }) => {
    const verb = forwards.isRunning(hostId, ruleId) ? "Stop" : "Start";
    const label = `${verb} forward: ${ruleLabel} (${hostLabel})`;
    return (
      <Command.Item
        key={`${hostId}/${ruleId}`}
        value={label}
        onSelect={() => {
          shell.setPaletteOpen(false);
          shell.toggleForward(hostId, ruleId);
        }}
        className={ITEM}
      >
        {label}
      </Command.Item>
    );
  });
}

const RULE_ITEMS = 50;

/**
 * "Run snippet" for those whose folder and name hold every word typed, at
 * most {@link SNIPPET_ITEMS}. O(S) per keystroke, S ≤ 2000.
 */
function SnippetItems({ shell, snippets }: { shell: AppShell; snippets: SnippetsStore }) {
  const all = useSyncExternalStore(snippets.subscribe, snippets.getSnapshot).snippets;
  const search = useCommandState((state) => state.search);
  const titled = useMemo(
    () =>
      all.map((s) => ({ id: s.id, title: snippetTitle(s), key: snippetTitle(s).toLowerCase() })),
    [all],
  );
  const words = search.toLowerCase().split(/\s+/u).filter(Boolean);
  return titled
    .filter((s) => words.every((w) => `run snippet: ${s.key}`.includes(w)))
    .slice(0, SNIPPET_ITEMS)
    .map(({ id, title }) => {
      const label = `Run snippet: ${title}`;
      return (
        <Command.Item
          key={id}
          // Unique, so two snippets with one name are told apart.
          value={`${label}\u0000${id}`}
          onSelect={() => {
            shell.setPaletteOpen(false);
            shell.chooseSnippet(id);
          }}
          className={ITEM}
        >
          {label}
        </Command.Item>
      );
    });
}

const SNIPPET_ITEMS = 50;

/** "Open workspace" for each saved one; at most 50, so cmdk filters them. */
function WorkspaceItems({ shell }: { shell: AppShell }) {
  const all = useSyncExternalStore(shell.workspaces.subscribe, shell.workspaces.getSnapshot);
  return all.map((workspace) => {
    const label = `Open workspace: ${workspace.name}`;
    return (
      <Command.Item
        key={workspace.id}
        value={`${label}\u0000${workspace.id}`}
        onSelect={() => {
          shell.setPaletteOpen(false);
          void shell.openWorkspace(workspace.id);
        }}
        className={ITEM}
      >
        {label}
      </Command.Item>
    );
  });
}
