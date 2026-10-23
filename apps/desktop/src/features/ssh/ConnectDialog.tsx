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

const FIELD = "field font-mono";

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
      className="panel modal w-[min(520px,90vw)] p-0"
    >
      <form onSubmit={submit} className="p-4">
        <h2 id={titleId} className="mb-3 font-semibold">
          Connect to SSH host
        </h2>
        <label className="mb-3 block">
          <span className="mb-1.5 block text-[13px] font-medium text-muted">Destination</span>
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
          <span className="mb-1.5 block text-[13px] font-medium text-muted">
            Jump hosts (optional, like ssh -J)
          </span>
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
          <p id={errorId} role="alert" className="mb-3 text-error">
            {error}
          </p>
        )}
        <p className="mb-3 text-xs opacity-60">
          Keys come from ssh-agent and ~/.ssh/id_*; host keys are checked against
          ~/.ssh/known_hosts.
        </p>
        <div className="flex justify-end gap-2">
          <button type="button" onClick={() => shell.setConnectOpen(false)} className="btn">
            Cancel
          </button>
          <button type="submit" className="btn btn-primary">
            Connect
          </button>
        </div>
      </form>
    </dialog>
  );
}
