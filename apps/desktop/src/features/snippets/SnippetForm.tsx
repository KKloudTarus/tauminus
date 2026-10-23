// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useId, useState } from "react";

import type { SnippetView } from "@/ipc/index.ts";

import { snippetTitle } from "./snippets-store.ts";

const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1 font-mono outline-none focus:border-current/50";

/** Asks for a snippet's placeholders, each starting at its default. */
export function SnippetForm({
  snippet,
  onRun,
  onClose,
}: {
  snippet: SnippetView;
  onRun: (values: Record<string, string>) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(snippet.variables.map((v) => [v.name, v.default ?? ""])),
  );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    onRun(values);
  };
  return (
    <div className="fixed inset-0 z-30 flex items-start justify-center bg-black/30 pt-[15vh]">
      <dialog
        open
        aria-labelledby={titleId}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        className="static m-0 w-[min(480px,92vw)] rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl"
      >
        <h2 id={titleId} className="mb-3 text-base font-medium">
          Run {snippetTitle(snippet)}
        </h2>
        <form onSubmit={submit} className="flex flex-col gap-2">
          {snippet.variables.map((v, i) => (
            <label key={v.name} className="flex flex-col gap-1">
              {v.name}
              <input
                className={FIELD}
                value={values[v.name] ?? ""}
                // Without a default, a blank value would run the command without it.
                required={v.default === null}
                autoFocus={i === 0}
                spellCheck={false}
                onChange={(e) => setValues((all) => ({ ...all, [v.name]: e.target.value }))}
              />
            </label>
          ))}
          <div className="mt-2 flex justify-end gap-2">
            <button type="button" className="rounded px-2 hover:bg-current/10" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="rounded px-2 hover:bg-current/10">
              Run
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}
