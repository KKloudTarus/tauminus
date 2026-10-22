// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useRef, useSyncExternalStore } from "react";

import { PromptDialog } from "@/features/ssh/PromptDialog.tsx";

import { ConnectFailure } from "./ConnectFailure.tsx";
import type { TerminalController } from "./controller.ts";
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
        className="min-h-0 flex-1 p-2"
        style={{ minWidth: 0, contain: "strict" }}
        data-testid="terminal-host"
      />
      {status.kind === "starting" && remote && !prompt && (
        <output className="absolute inset-x-0 top-0 px-3 py-1 text-xs opacity-70">
          Connecting to {controller.defaultTitle}…
        </output>
      )}
      {status.kind === "exited" && (
        <output className="block border-t border-current/10 px-3 py-1 text-xs opacity-80">
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
          <output className="block border-t border-current/10 px-3 py-1 text-xs opacity-80">
            Could not start the shell: {status.message}
          </output>
        ))}
      {guarded && (
        <output
          data-testid="prod-status"
          className="block bg-[var(--prod)] px-3 py-0.5 text-xs font-medium text-white"
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
