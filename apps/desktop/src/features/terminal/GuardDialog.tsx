// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useId, useRef } from "react";

import type { GuardRequest } from "./command-guard.ts";
import { visibleText } from "./visible-text.ts";

interface GuardDialogProps {
  request: GuardRequest;
  host: string;
  onAnswer: (run: boolean) => void;
}

/**
 * Asks before a command runs, or a paste lands, on a guarded host. Shows the
 * host and the exact text. Cancel has focus, so a reflexive Enter cancels;
 * Escape cancels too. Covers only its pane.
 */
export function GuardDialog({ request, host, onAnswer }: GuardDialogProps) {
  const titleId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);
  // Each question mounts a new dialog: the controller clears one before the next.
  useEffect(() => {
    cancelRef.current?.focus();
  }, []);
  const lines = request.text.split("\n").filter((line) => line.trim() !== "").length;
  const title =
    request.kind === "paste"
      ? `Paste ${lines === 1 ? "1 line" : `${lines} lines`} into ${host}?`
      : `Run this on ${host}?`;
  return (
    <div className="absolute inset-0 z-20 flex items-center justify-center bg-terminal/80 p-4">
      <dialog
        open
        aria-labelledby={titleId}
        data-testid="guard-dialog"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onAnswer(false);
          }
        }}
        className="panel static m-0 w-[min(560px,100%)] border-2 border-prod p-4 shadow-[0_0_0_4px_var(--prod-soft),var(--shadow-lg)]"
      >
        <h2 id={titleId} className="mb-1 font-semibold">
          {title}
        </h2>
        <p className="mb-3 text-xs text-muted">
          <span className="chip chip-prod">PROD</span> {host}
        </p>
        <pre
          data-testid="guard-text"
          className="mb-3 max-h-48 overflow-auto rounded-lg border border-line bg-terminal p-2.5 font-mono text-xs break-all whitespace-pre-wrap"
        >
          {visibleText(request.text)}
        </pre>
        {request.findings.length > 0 ? (
          <ul className="mb-4 list-disc space-y-1 pl-5" data-testid="guard-findings">
            {request.findings.map((finding) => (
              <li key={`${finding.rule}:${finding.command}`}>
                <span className="font-medium">{capitalize(finding.reason)}</span>
                {finding.command && (
                  <>
                    : <code className="break-all">{visibleText(finding.command)}</code>
                  </>
                )}
              </li>
            ))}
          </ul>
        ) : (
          <p className="mb-4 opacity-80">Each line runs as it arrives.</p>
        )}
        <div className="flex justify-end gap-2">
          <button ref={cancelRef} type="button" onClick={() => onAnswer(false)} className="btn">
            Cancel
          </button>
          <button
            type="button"
            onClick={() => onAnswer(true)}
            className="btn border-transparent bg-prod-fill text-white hover:!bg-prod-fill/90"
          >
            {request.kind === "paste" ? "Paste" : "Run"}
          </button>
        </div>
      </dialog>
    </div>
  );
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}
