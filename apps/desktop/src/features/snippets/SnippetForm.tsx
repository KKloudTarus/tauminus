// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useId, useState } from "react";

import type { SnippetView } from "@/ipc/index.ts";

import { snippetTitle } from "./snippets-store.ts";

const FIELD = "field font-mono";

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
    <div className="overlay z-30">
      <dialog
        open
        aria-labelledby={titleId}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        className="panel static m-0 w-[min(480px,92vw)] p-4"
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
            <button type="button" className="btn btn-ghost btn-xs" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-ghost btn-xs">
              Run
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}
