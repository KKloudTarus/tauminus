// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useRef, useSyncExternalStore } from "react";

import type { TerminalController } from "./controller.ts";

interface TerminalViewProps {
  controller: TerminalController;
  className?: string;
}

/** Shows a terminal. Mounting attaches it; unmounting detaches it and keeps the session. */
export function TerminalView({ controller, className }: TerminalViewProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const status = useSyncExternalStore(controller.subscribe, controller.getStatus);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) {
      return undefined;
    }
    controller.attach(host);
    return () => controller.detach();
  }, [controller]);

  return (
    <div className={`relative flex flex-col ${className ?? ""}`}>
      <div ref={hostRef} className="min-h-0 flex-1 p-2" data-testid="terminal-host" />
      {(status.kind === "exited" || status.kind === "failed") && (
        <output className="block border-t border-current/10 px-3 py-1 text-xs opacity-80">
          {status.kind === "exited"
            ? `Process exited${status.code === null ? "" : ` with code ${status.code}`}`
            : `Could not start the shell: ${status.message}`}
        </output>
      )}
    </div>
  );
}
