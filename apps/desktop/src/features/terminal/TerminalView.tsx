// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import { PromptDialog } from "@/features/ssh/PromptDialog.tsx";

import { ConnectFailure } from "./ConnectFailure.tsx";
import type { CompletionState, TerminalController } from "./controller.ts";
import { GuardDialog } from "./GuardDialog.tsx";

interface TerminalViewProps {
  controller: TerminalController;
  className?: string;
  onUnlockVault?: (retry: () => void) => void;
}

/** Shows a terminal. Mounting attaches it; unmounting detaches it and keeps the session. */
export function TerminalView({ controller, className, onUnlockVault }: TerminalViewProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const status = useSyncExternalStore(controller.subscribe, controller.getStatus);
  const prompt = useSyncExternalStore(controller.subscribe, controller.getPrompt);
  const guardRequest = useSyncExternalStore(controller.subscribe, controller.getGuardRequest);
  const guarded = useSyncExternalStore(controller.subscribeGuard, () => controller.guarded);
  const recording = useSyncExternalStore(controller.subscribe, controller.getRecording);
  const fill = useSyncExternalStore(controller.subscribe, controller.getFill);
  const completion = useSyncExternalStore(controller.subscribe, controller.getCompletion);
  const remote = controller.spec.kind !== "local";

  useEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return undefined;
    }
    controller.attach(host);
    return () => controller.detach();
  }, [controller]);

  return (
    <div className={`relative flex flex-col ${className ?? ""}`} style={{ minWidth: 0 }}>
      {/*
        Sized by the layout alone: with size containment the terminal's own
        width cannot widen the pane, which fit would answer with more columns
        and a wider terminal, a loop that ran 30 times a second on macOS.
        Inline, so no stylesheet can lose it.
      */}
      <div
        ref={hostRef}
        className="min-h-0 flex-1 px-3 pt-2 pb-1"
        style={{ minWidth: 0, contain: "strict" }}
        data-testid="terminal-host"
      />
      {status.kind === "starting" && remote && !prompt && (
        <output className="absolute top-2 right-3 flex items-center gap-2 rounded-full border border-line bg-surface-3 px-3 py-1 text-xs text-muted shadow-lg">
          <span aria-hidden className="size-1.5 rounded-full bg-accent" />
          Connecting to {controller.defaultTitle}…
        </output>
      )}
      {status.kind === "reconnecting" && (
        <Reconnecting
          target={controller.defaultTitle}
          attempt={status.attempt}
          retryAt={status.retryAt}
          queued={status.queued === true}
          onNow={() => controller.reconnectNow()}
          onStop={() => controller.stopReconnecting()}
        />
      )}
      {status.kind === "exited" && (
        <output className="block border-t border-line bg-surface-1 px-3 py-1.5 text-xs text-muted">
          {remote ? "Connection closed" : "Process exited"}
          {status.code === null ? "" : ` with code ${status.code}`}
        </output>
      )}
      {status.kind === "failed" &&
        (remote ? (
          <ConnectFailure
            target={controller.defaultTitle}
            code={status.code}
            message={status.message}
            onRetry={() => controller.retry()}
            {...(onUnlockVault ? { onUnlockVault } : {})}
          />
        ) : (
          <output className="block border-t border-line bg-surface-1 px-3 py-1.5 text-xs text-error">
            Could not start the shell: {status.message}
          </output>
        ))}
      {(fill?.kind === "echoed" || fill?.kind === "failed") && (
        <p
          role="alert"
          data-testid={fill.kind === "echoed" ? "password-echoed" : "password-failed"}
          className="absolute right-3 bottom-2 max-w-[70%] rounded bg-amber-500 px-2 py-1 text-xs text-black shadow"
        >
          {fill.kind === "echoed"
            ? "The host showed what was typed: it may not have asked for a password. Ctrl+U clears the line before Enter."
            : `No password typed: ${fill.message}`}
        </p>
      )}
      {completion && <CompletionList controller={controller} completion={completion} />}
      {fill?.kind === "offer" && (
        <button
          type="button"
          tabIndex={-1}
          data-testid="password-fill"
          title={`Types the vault password for ${fill.login}, without Enter`}
          className="absolute right-3 bottom-2 rounded border border-current/20 bg-background px-2 py-1 text-xs shadow hover:bg-current/10"
          onClick={() => {
            void controller.fillPassword().then((outcome) => {
              if (outcome === "locked") {
                onUnlockVault?.(() => void controller.fillPassword());
              }
            });
            controller.focus();
          }}
        >
          {fill.identity === null
            ? `Type the vault password for ${fill.login}`
            : `Type the password of ${fill.identity} (${fill.login})`}
        </button>
      )}
      {recording && (
        <output
          data-testid="recording-status"
          className="absolute top-1 right-3 rounded bg-red-600 px-1.5 text-[10px] font-semibold text-white"
          title="This pane is being recorded"
        >
          ● REC
        </output>
      )}
      {guarded && (
        <output
          data-testid="prod-status"
          className="block bg-prod-fill px-3 py-1 text-xs font-semibold text-white"
        >
          PROD · {controller.defaultTitle} · commands are checked before they run
        </output>
      )}
      {prompt && <PromptDialog prompt={prompt} onAnswer={(answer) => controller.answer(answer)} />}
      {guardRequest && (
        <GuardDialog
          request={guardRequest}
          host={controller.defaultTitle}
          onAnswer={(run) => controller.answerGuard(run)}
        />
      )}
    </div>
  );
}

/** The connection is gone; a new one is tried, counting down to it. */
function Reconnecting({
  target,
  attempt,
  retryAt,
  queued,
  onNow,
  onStop,
}: {
  target: string;
  attempt: number;
  retryAt: number;
  queued: boolean;
  onNow: () => void;
  onStop: () => void;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const seconds = Math.max(0, Math.ceil((retryAt - now) / 1000));
  return (
    <output
      data-testid="reconnecting"
      className="flex items-center gap-2 border-t border-line bg-surface-1 px-3 py-1 text-xs"
    >
      <span className="min-w-0 flex-1">
        Connection to {target} lost.{" "}
        {queued
          ? "Waiting for other panes of this host to connect first."
          : `Connecting again ${seconds > 0 ? `in ${seconds} s` : "now"}${attempt > 1 ? ` (try ${attempt})` : ""}.`}
      </span>
      <button type="button" className="btn btn-ghost btn-xs" onClick={onNow}>
        Reconnect now
      </button>
      <button type="button" className="btn btn-ghost btn-xs" onClick={onStop}>
        Stop
      </button>
    </output>
  );
}

/**
 * The completion list, under the cursor's cell. The keys stay in the
 * terminal, which moves the selection; a click takes an entry.
 */
function CompletionList({
  controller,
  completion,
}: {
  controller: TerminalController;
  completion: CompletionState;
}) {
  const cell = controller.cellSize();
  return (
    <ul
      aria-label="Completions"
      data-testid="completion"
      className="absolute z-10 max-h-64 min-w-48 overflow-auto rounded border border-current/20 bg-background py-1 font-mono text-xs shadow-lg"
      style={{
        left: Math.max(0, (completion.column - completion.current.length) * cell.width),
        top: (completion.row + 1) * cell.height,
      }}
    >
      {completion.items.map((item, index) => (
        <li key={`${item.kind}:${item.name}`}>
          <button
            type="button"
            tabIndex={-1}
            data-selected={index === completion.selected}
            className={`flex w-full gap-3 px-2 py-0.5 text-left ${
              index === completion.selected ? "bg-current/15" : "hover:bg-current/10"
            }`}
            onMouseDown={(event) => {
              event.preventDefault();
              controller.acceptCompletion(index);
              controller.focus();
            }}
          >
            <span className="shrink-0">{item.name}</span>
            {item.description && <span className="truncate opacity-60">{item.description}</span>}
          </button>
        </li>
      ))}
    </ul>
  );
}
