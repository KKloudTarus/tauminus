// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { memo, useEffect, useId, useRef, useSyncExternalStore } from "react";

import { visibleText } from "@/features/terminal/visible-text.ts";

import type { Capture, CapturesStore } from "./captures-store.ts";

const BUTTON = "rounded px-2 py-0.5 hover:bg-current/10 disabled:opacity-40";

/** The lines capture rules kept, newest first: read, copy, or clear them. */
export function CapturesDialog({
  store,
  onCopy,
  onClose,
}: {
  store: CapturesStore;
  onCopy: (text: string) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const captures = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => dialog.current?.focus(), []);
  return (
    <div className="fixed inset-0 z-20 flex items-start justify-center bg-black/30 pt-[8vh]">
      <dialog
        ref={dialog}
        open
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        className="static m-0 max-h-[80vh] w-[min(760px,94vw)] overflow-y-auto rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl outline-none"
      >
        <div className="mb-3 flex items-center justify-between gap-2">
          <h2 id={titleId} className="text-base font-medium">
            Captured lines
          </h2>
          <div className="flex gap-1">
            <button
              type="button"
              className={BUTTON}
              disabled={captures.length === 0}
              onClick={() => onCopy(captures.map((c) => c.text).join("\n"))}
            >
              Copy all
            </button>
            <button
              type="button"
              className={BUTTON}
              disabled={captures.length === 0}
              onClick={() => store.clear()}
            >
              Clear
            </button>
            <button type="button" className={BUTTON} onClick={onClose}>
              Close
            </button>
          </div>
        </div>
        {captures.length === 0 && (
          <p className="opacity-70">
            Nothing captured yet. A rule whose action is “Keep the line” keeps each line of output
            it matches here, until this window closes.
          </p>
        )}
        <ol aria-label="Captured lines" className="flex flex-col gap-1">
          {captures.map((capture) => (
            <CaptureRow key={capture.id} capture={capture} onCopy={onCopy} />
          ))}
        </ol>
      </dialog>
    </div>
  );
}

/** A capture never changes: a list grown by a scan draws only the new rows. */
const CaptureRow = memo(function CaptureRow({
  capture,
  onCopy,
}: {
  capture: Capture;
  onCopy: (text: string) => void;
}) {
  return (
    <li className="flex items-start gap-2">
      <span className="w-16 shrink-0 text-xs tabular-nums opacity-60">
        {new Date(capture.at).toLocaleTimeString()}
      </span>
      <span className="min-w-0 flex-1">
        <code className="block break-all">{visibleText(capture.text)}</code>
        <span className="text-xs opacity-60">
          {capture.rule} · {capture.pane}
        </span>
      </span>
      <button
        type="button"
        className={BUTTON}
        aria-label={`Copy ${capture.text}`}
        onClick={() => onCopy(capture.text)}
      >
        Copy
      </button>
    </li>
  );
});
