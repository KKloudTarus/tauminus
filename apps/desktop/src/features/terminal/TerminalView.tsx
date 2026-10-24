// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import { PromptDialog } from "@/features/ssh/PromptDialog.tsx";

import { ConnectFailure } from "./ConnectFailure.tsx";
import type { CompletionState, TerminalController } from "./controller.ts";
import { GuardDialog } from "./GuardDialog.tsx";
import { type ReceiveState, SETTLE, formatBytes } from "./receive.ts";

interface TerminalViewProps {
  controller: TerminalController;
  className?: string;
  onUnlockVault?: (retry: () => void) => void;
}

/** The host's padding above and beside the cells; the completion layer copies it. */
const HOST_PAD = "px-3 pt-2";

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
  const receive = useSyncExternalStore(controller.subscribe, controller.getReceive);
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
        className={`min-h-0 flex-1 ${HOST_PAD} pb-1`}
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
          className="absolute right-3 bottom-2 max-w-[70%] rounded-md bg-warn px-2 py-1 text-xs font-medium text-black shadow"
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
          className="btn btn-sm absolute right-3 bottom-2 bg-background text-xs shadow"
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
          // Below the Broadcast badge when the pane has one, so both show.
          className="chip chip-prod absolute top-2 right-3 in-data-[broadcast=true]:top-9"
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
      {receive && <Receive controller={controller} state={receive} />}
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

/**
 * Files the host sends with `sz`: asked first, then their progress, then
 * where they went. Nothing goes back to the host before Receive.
 */
function Receive({ controller, state }: { controller: TerminalController; state: ReceiveState }) {
  // An offer's buttons wait a moment: a click meant for what was there
  // before must not answer it.
  const shownAt = state.kind === "offered" ? state.shownAt : 0;
  const [settledFor, setSettledFor] = useState(0);
  useEffect(() => {
    if (shownAt === 0) return undefined;
    const timer = setTimeout(() => setSettledFor(shownAt), shownAt + SETTLE - performance.now());
    return () => clearTimeout(timer);
  }, [shownAt]);
  const settled = settledFor === shownAt;
  return (
    <output
      data-testid="receive"
      className="flex items-center gap-2 border-t border-line bg-surface-1 px-3 py-1 text-xs"
    >
      {state.kind === "offered" && (
        <>
          <span className="min-w-0 flex-1">
            {controller.defaultTitle} wants to send files (Zmodem). They go to Downloads/Tauminus;
            Ctrl+C declines.
          </span>
          <button
            type="button"
            className="btn btn-primary btn-xs"
            disabled={state.answered || !settled}
            onClick={() => controller.answerReceive(true)}
          >
            Receive
          </button>
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            disabled={state.answered || !settled}
            onClick={() => controller.answerReceive(false)}
          >
            Decline
          </button>
        </>
      )}
      {state.kind === "receiving" && (
        <>
          <span className="min-w-0 flex-1 truncate">
            Receiving {state.name ?? "files"}
            {state.size === null
              ? ` · ${formatBytes(state.bytes)}`
              : ` · ${formatBytes(state.bytes)} of ${formatBytes(state.size)}`}
            <span className="text-faint"> · Ctrl+C stops</span>
          </span>
          {state.size !== null && state.size > 0 && (
            <progress
              className="h-1 w-24"
              max={state.size}
              value={Math.min(state.bytes, state.size)}
              aria-label="Received"
            />
          )}
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            onClick={() => controller.cancelReceive()}
          >
            Stop
          </button>
        </>
      )}
      {state.kind === "finished" && (
        <>
          <span className="min-w-0 flex-1 truncate">
            {state.files.length === 0
              ? "No file was received"
              : `Received ${names(state.files, state.moreFiles)}`}
            {state.skipped.length > 0 && (
              <span className="text-error">
                {" "}
                · skipped, names that cannot be saved: {names(state.skipped, state.moreSkipped)}
              </span>
            )}
          </span>
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            onClick={() => controller.revealReceived()}
          >
            Show folder
          </button>
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            onClick={() => controller.dismissReceive()}
          >
            Close
          </button>
        </>
      )}
      {state.kind === "failed" && (
        <>
          <span className="min-w-0 flex-1 text-error">
            Transfer stopped: {state.reason}. Enter brings the prompt back.
          </span>
          <button
            type="button"
            className="btn btn-ghost btn-xs"
            onClick={() => controller.dismissReceive()}
          >
            Close
          </button>
        </>
      )}
    </output>
  );
}

/** `a, b and 3 more`. */
function names(list: string[], more: number): string {
  return more > 0 ? `${list.join(", ")} and ${more} more` : list.join(", ");
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
  const [now, setNow] = useState(() => performance.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(performance.now()), 1000);
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
  const start = Math.max(0, (completion.column - completion.current.length) * cell.width);
  return (
    // A layer padded like the host starts where the cells do.
    <div className={`pointer-events-none absolute inset-x-0 top-0 z-10 ${HOST_PAD}`}>
      <div className="relative">
        <ul
          aria-label="Completions"
          data-testid="completion"
          className="pointer-events-auto absolute max-h-64 min-w-48 overflow-auto rounded-lg border border-line-strong bg-surface-2 p-1 font-mono text-xs shadow-lg"
          style={{
            // Under the word, but never past the right edge: near it, the list
            // shifts left to keep its 12rem, and it is never wider than the view.
            left: `min(${start}px, max(0px, 100% - 12rem))`,
            maxWidth: `calc(100% - min(${start}px, max(0px, 100% - 12rem)))`,
            top: (completion.row + 1) * cell.height,
          }}
        >
          {completion.items.map((item, index) => (
            <li key={`${item.kind}:${item.name}`}>
              <button
                type="button"
                tabIndex={-1}
                data-selected={index === completion.selected}
                className={`flex w-full gap-3 rounded px-2 py-0.5 text-left ${
                  index === completion.selected
                    ? "bg-accent-soft text-foreground"
                    : "hover:bg-surface-3"
                }`}
                onMouseDown={(event) => {
                  event.preventDefault();
                  controller.acceptCompletion(index);
                  controller.focus();
                }}
              >
                <span className="shrink-0">{item.name}</span>
                {item.description && (
                  <span className="min-w-0 truncate text-faint">{item.description}</span>
                )}
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
