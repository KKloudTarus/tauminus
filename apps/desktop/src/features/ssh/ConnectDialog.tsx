// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";

import type { AppShell } from "@/app/shell.ts";

import { parseDestination, parseJumps } from "./destination.ts";

/** Quick connect: a destination as `ssh` takes it, and optional jump hosts. */
export function ConnectDialog({ shell }: { shell: AppShell }) {
  const { connectOpen } = useSyncExternalStore(shell.subscribe, shell.getState);
  if (!connectOpen) return null;
  return <ConnectForm shell={shell} />;
}

const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1.5 font-mono outline-none focus:border-current/50";

function ConnectForm({ shell }: { shell: AppShell }) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [destination, setDestination] = useState("");
  const [jumps, setJumps] = useState("");
  const [error, setError] = useState<string | undefined>();
  const titleId = useId();
  const errorId = useId();

  // A modal dialog traps focus, puts the rest of the page out of reach, and
  // closes on Escape without any code here.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const parsed = parseDestination(destination);
    if (!parsed.ok) {
      setError(parsed.error);
      return;
    }
    const hops = parseJumps(jumps);
    if (!hops.ok) {
      setError(`Jump host: ${hops.error}`);
      return;
    }
    shell.setConnectOpen(false);
    const target = {
      destination: parsed.value,
      jumps: hops.value,
      route: { kind: "direct" as const },
    };
    if (!shell.connect({ kind: "ssh", target })) {
      console.warn("pane limit reached; not connecting");
    }
  };

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={() => shell.setConnectOpen(false)}
      className="top-[15%] m-0 mx-auto w-[min(520px,90vw)] rounded-lg border border-current/15 bg-background p-0 text-sm text-foreground shadow-xl backdrop:bg-black/30"
    >
      <form onSubmit={submit} className="p-4">
        <h2 id={titleId} className="mb-3 font-semibold">
          Connect to SSH host
        </h2>
        <label className="mb-3 block">
          <span className="mb-1 block opacity-80">Destination</span>
          <input
            value={destination}
            onChange={(event) => {
              setDestination(event.target.value);
              setError(undefined);
            }}
            placeholder="user@host:port"
            spellCheck={false}
            autoCapitalize="off"
            aria-invalid={error !== undefined}
            aria-describedby={error ? errorId : undefined}
            className={FIELD}
          />
        </label>
        <label className="mb-3 block">
          <span className="mb-1 block opacity-80">Jump hosts (optional, like ssh -J)</span>
          <input
            value={jumps}
            onChange={(event) => {
              setJumps(event.target.value);
              setError(undefined);
            }}
            placeholder="user@bastion, user@inner"
            spellCheck={false}
            autoCapitalize="off"
            className={FIELD}
          />
        </label>
        {error && (
          <p id={errorId} role="alert" className="mb-3 text-red-600 dark:text-red-400">
            {error}
          </p>
        )}
        <p className="mb-3 text-xs opacity-60">
          Keys come from ssh-agent and ~/.ssh/id_*; host keys are checked against
          ~/.ssh/known_hosts.
        </p>
        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={() => shell.setConnectOpen(false)}
            className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
          >
            Cancel
          </button>
          <button
            type="submit"
            className="rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90"
          >
            Connect
          </button>
        </div>
      </form>
    </dialog>
  );
}
