// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { ForwardKindSpec } from "@/ipc/index.ts";

import { type RuleRow, isKind, newRow, opensToNetwork } from "./rules.ts";

const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1 outline-none focus:border-current/50";

const KINDS: { kind: ForwardKindSpec; name: string; hint: string }[] = [
  { kind: "local", name: "Local (-L)", hint: "Listen here, connect from the server" },
  { kind: "remote", name: "Remote (-R)", hint: "Listen on the server, connect from here" },
  {
    kind: "dynamic",
    name: "SOCKS (-D)",
    hint: "A SOCKS proxy here that reaches out from the server",
  },
];

/** A host's forwarding rules in the host editor. */
export function ForwardFields({
  rows,
  onChange,
}: {
  rows: readonly RuleRow[];
  onChange: (rows: RuleRow[]) => void;
}) {
  const update = (index: number, patch: Partial<RuleRow>) =>
    onChange(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  return (
    <details className="mb-3 rounded border border-current/15 px-3 py-2" open={rows.length > 0}>
      <summary className="cursor-pointer select-none">
        Port forwarding{rows.length > 0 ? ` (${rows.length})` : ""}
      </summary>
      <div className="mt-2 flex flex-col gap-3">
        {rows.map((row, index) => (
          <RuleFields
            key={row.uid}
            row={row}
            onChange={(patch) => update(index, patch)}
            onRemove={() => onChange(rows.filter((_, i) => i !== index))}
          />
        ))}
        <div className="flex flex-wrap gap-2">
          {KINDS.map(({ kind, name, hint }) => (
            <button
              key={kind}
              type="button"
              title={hint}
              className="rounded border border-current/20 px-2 py-1 text-xs hover:bg-current/10"
              onClick={() => onChange([...rows, newRow(kind)])}
            >
              Add {name}
            </button>
          ))}
        </div>
        <p className="text-xs opacity-60">
          Port 0 lets the system pick one. Start and stop rules from the command palette.
        </p>
      </div>
    </details>
  );
}

function RuleFields({
  row,
  onChange,
  onRemove,
}: {
  row: RuleRow;
  onChange: (patch: Partial<RuleRow>) => void;
  onRemove: () => void;
}) {
  const custom = row.bind !== "loopback" && row.bind !== "any";
  const side = row.kind === "remote" ? "the server" : "this machine";
  return (
    <fieldset className="rounded border border-current/10 p-2" aria-label={row.label || "New rule"}>
      <div className="mb-2 grid grid-cols-[1fr_9rem_auto] gap-2">
        <input
          aria-label="Rule name"
          value={row.label}
          onChange={(e) => onChange({ label: e.target.value })}
          placeholder="Name, as postgres"
          className={FIELD}
        />
        <select
          aria-label="Kind"
          value={row.kind}
          onChange={(e) => {
            const kind = e.target.value;
            if (isKind(kind)) onChange({ kind });
          }}
          className={FIELD}
        >
          {KINDS.map(({ kind, name }) => (
            <option key={kind} value={kind}>
              {name}
            </option>
          ))}
        </select>
        <button
          type="button"
          aria-label="Remove rule"
          className="rounded px-2 hover:bg-current/10"
          onClick={onRemove}
        >
          ×
        </button>
      </div>
      <div className="mb-2 grid grid-cols-[9rem_1fr_6rem] gap-2">
        <select
          aria-label={`Listen on ${side}`}
          value={custom ? "custom" : row.bind}
          onChange={(e) => onChange({ bind: e.target.value === "custom" ? "" : e.target.value })}
          className={FIELD}
        >
          <option value="loopback">Only {side}</option>
          <option value="any">Every interface</option>
          <option value="custom">One address</option>
        </select>
        <input
          aria-label="Listen address"
          disabled={!custom}
          value={custom ? row.bind : ""}
          onChange={(e) => onChange({ bind: e.target.value })}
          placeholder={custom ? "192.168.1.5" : ""}
          spellCheck={false}
          className={`${FIELD} font-mono`}
        />
        <input
          aria-label="Listen port"
          value={row.port}
          onChange={(e) => onChange({ port: e.target.value })}
          inputMode="numeric"
          placeholder="port"
          className={FIELD}
        />
      </div>
      {row.kind === "remote" && (
        <p className="mb-2 text-xs opacity-60">
          The server&apos;s GatewayPorts setting has the last word on where it listens.
        </p>
      )}
      {row.kind !== "dynamic" && (
        <div className="mb-2 grid grid-cols-[1fr_6rem] gap-2">
          <input
            aria-label={
              row.kind === "local"
                ? "Destination, as the server sees it"
                : "Destination, as this machine sees it"
            }
            value={row.toHost}
            onChange={(e) => onChange({ toHost: e.target.value })}
            placeholder="localhost"
            spellCheck={false}
            className={`${FIELD} font-mono`}
          />
          <input
            aria-label="Destination port"
            value={row.toPort}
            onChange={(e) => onChange({ toPort: e.target.value })}
            inputMode="numeric"
            placeholder="port"
            className={FIELD}
          />
        </div>
      )}
      <label className="flex items-center gap-2 text-xs">
        <input
          type="checkbox"
          checked={row.autoStart}
          onChange={(e) => onChange({ autoStart: e.target.checked })}
        />
        <span>Start when the host connects</span>
      </label>
      {row.bind.trim() && opensToNetwork(row.bind) && (
        <p className="mt-1 text-xs text-red-600 dark:text-red-400">
          Other machines on the network can use this tunnel.
        </p>
      )}
    </fieldset>
  );
}
