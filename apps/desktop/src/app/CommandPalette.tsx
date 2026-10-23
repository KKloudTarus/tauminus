// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Command, useCommandState } from "cmdk";
import { useMemo, useSyncExternalStore } from "react";

import type { ForwardsStore } from "@/features/forwards/forwards-store.ts";
import { matchRules, ruleChoices } from "@/features/forwards/rules.ts";
import { ACTIONS, type Action, displayChord } from "@/features/keymap/keymap.ts";
import type { HostView } from "@/ipc/index.ts";

import { ACTION_LABELS, type AppShell } from "./shell.ts";

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
      overlayClassName="fixed inset-0 bg-black/30"
      contentClassName="fixed top-[15%] left-1/2 w-[min(560px,90vw)] -translate-x-1/2 overflow-hidden rounded-lg border border-current/15 bg-background text-foreground shadow-xl"
    >
      <Command.Input
        placeholder="Type a command"
        className="w-full border-b border-current/10 bg-transparent px-4 py-3 text-sm outline-none"
      />
      <Command.List className="max-h-80 overflow-y-auto p-1 text-sm">
        <Command.Empty className="px-3 py-2 opacity-60">No matching command</Command.Empty>
        {choices.map((action) => {
          const chord = shell.keymap.chords(action)[0];
          return (
            <Command.Item
              key={action}
              value={ACTION_LABELS[action]}
              onSelect={() => run(action)}
              className="flex cursor-default items-center justify-between rounded px-3 py-2 data-[selected=true]:bg-current/10"
            >
              <span>{ACTION_LABELS[action]}</span>
              {chord && (
                <kbd className="text-xs opacity-60">{displayChord(chord, shell.platform)}</kbd>
              )}
            </Command.Item>
          );
        })}
        {paletteOpen && shell.forwards && <RuleItems shell={shell} forwards={shell.forwards} />}
      </Command.List>
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
        className="flex cursor-default items-center rounded px-3 py-2 data-[selected=true]:bg-current/10"
      >
        {label}
      </Command.Item>
    );
  });
}

const RULE_ITEMS = 50;
