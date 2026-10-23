// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { memo, useEffect, useId, useRef, useSyncExternalStore } from "react";

import { PromptDialog } from "@/features/ssh/PromptDialog.tsx";
import type { ForwardSpec, ForwardState, HostView } from "@/ipc/index.ts";

import { type ForwardsStore, ruleKey } from "./forwards-store.ts";
import { describeRule, opensToNetwork } from "./rules.ts";

/** Every saved rule, by host, with what it is doing and start and stop. */
export function ForwardsDialog({
  store,
  hosts,
  onClose,
}: {
  store: ForwardsStore;
  hosts: readonly HostView[];
  onClose: () => void;
}) {
  const titleId = useId();
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => store.watch(), [store]);
  useEffect(() => dialog.current?.focus(), []);
  // Forwards run over SSH; a host switched to Telnet keeps its rules unused.
  const withRules = hosts.filter((host) => host.protocol === "ssh" && host.forwards.length > 0);

  return (
    <div className="fixed inset-0 z-20 flex items-start justify-center bg-black/30 pt-[10vh]">
      <dialog
        ref={dialog}
        open
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape" && !state.prompt) {
            event.preventDefault();
            onClose();
          }
        }}
        className="static m-0 max-h-[75vh] w-[min(640px,92vw)] overflow-y-auto rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl outline-none"
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 id={titleId} className="text-base font-medium">
            Port forwarding
          </h2>
          <button type="button" className="rounded px-2 hover:bg-current/10" onClick={onClose}>
            Close
          </button>
        </div>
        {withRules.length === 0 ? (
          <p className="opacity-70">
            No saved rules. Add one under Port forwarding when you edit a host.
          </p>
        ) : (
          withRules.map((host) => (
            <section key={host.id} aria-label={host.label} className="mb-4">
              <h3 className="mb-1 font-medium">{host.label}</h3>
              <ul className="flex flex-col gap-1">
                {host.forwards.map((rule) => {
                  const id = rule.id ?? "";
                  const key = ruleKey(host.id, id);
                  return (
                    <RuleItem
                      key={id}
                      hostId={host.id}
                      rule={rule}
                      running={state.running.get(key)}
                      starting={state.busy.has(key) || state.running.get(key)?.starting === true}
                      error={state.errors.get(key)}
                      store={store}
                    />
                  );
                })}
              </ul>
            </section>
          ))
        )}
        {state.prompt && (
          <PromptDialog prompt={state.prompt.prompt} onAnswer={state.prompt.answer} />
        )}
      </dialog>
    </div>
  );
}

/** One rule's line. Renders again only when its own status changes. */
const RuleItem = memo(function RuleItem({
  hostId,
  rule,
  running,
  starting,
  error,
  store,
}: {
  hostId: string;
  rule: ForwardSpec;
  running: ForwardState | undefined;
  starting: boolean;
  error: string | undefined;
  store: ForwardsStore;
}) {
  const listening = running?.port ?? null;
  const problem = starting ? undefined : (error ?? running?.ended ?? undefined);
  return (
    <li className="flex [contain-intrinsic-size:auto_64px] [content-visibility:auto] items-center justify-between gap-3 rounded border border-current/10 px-3 py-2">
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span
            aria-hidden
            className={`size-2 rounded-full ${listening != null ? "bg-green-500" : problem ? "bg-red-500" : "bg-current/30"}`}
          />
          <span className="font-medium">{rule.label}</span>
          {opensToNetwork(rule.bind) && (
            <span className="rounded bg-red-500/15 px-1 text-xs text-red-600 dark:text-red-400">
              network
            </span>
          )}
        </div>
        <div className="truncate font-mono text-xs opacity-70">{describeRule(rule, listening)}</div>
        <output className="block text-xs opacity-70">
          {listening != null
            ? listeningText(listening, running)
            : starting
              ? "Starting…"
              : problem
                ? problem
                : "Stopped"}
        </output>
      </div>
      <button
        type="button"
        className="shrink-0 rounded border border-current/20 px-3 py-1 hover:bg-current/10"
        onClick={() => void store.toggle(hostId, rule.id ?? "")}
      >
        {listening != null || starting ? "Stop" : "Start"}
      </button>
    </li>
  );
});

/** "Listening on 15432; 2 open, 9 so far, 1 reset for falling behind". */
function listeningText(port: number, running: ForwardState | undefined): string {
  const open = running?.open ?? 0;
  const total = running?.total ?? 0;
  const resets = running?.resets ?? 0;
  const reset =
    resets > 0 ? `, ${resets} reset for falling behind (a client that stopped reading)` : "";
  return `Listening on ${port}; ${open} open, ${total} so far${reset}`;
}
