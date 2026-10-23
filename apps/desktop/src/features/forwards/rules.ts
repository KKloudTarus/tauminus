// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { ForwardKindSpec, ForwardSpec, HostView } from "@/ipc/index.ts";

/** One rule as the host editor holds it: text fields, checked on save. */
export interface RuleRow {
  /** Keys the row in a list while it has no id. */
  readonly uid: number;
  /** `null` for a rule not saved yet. */
  id: string | null;
  label: string;
  kind: ForwardKindSpec;
  /** `loopback`, `any`, or an IP address. */
  bind: string;
  port: string;
  toHost: string;
  toPort: string;
  autoStart: boolean;
}

let nextUid = 0;

export function ruleRow(spec: ForwardSpec): RuleRow {
  return {
    uid: nextUid++,
    id: spec.id ?? null,
    label: spec.label,
    kind: spec.kind,
    bind: spec.bind,
    port: String(spec.port),
    toHost: spec.toHost ?? "",
    toPort: spec.toPort == null ? "" : String(spec.toPort),
    autoStart: spec.autoStart ?? false,
  };
}

export function newRow(kind: ForwardKindSpec): RuleRow {
  return {
    uid: nextUid++,
    id: null,
    label: "",
    kind,
    bind: "loopback",
    port: kind === "dynamic" ? "1080" : "",
    toHost: kind === "dynamic" ? "" : "localhost",
    toPort: "",
    autoStart: false,
  };
}

/** A port from text: 0 to 65535 for a listener, 1 up for a destination. */
function portOf(text: string, min: number): number | undefined {
  const trimmed = text.trim();
  if (!/^\d{1,5}$/.test(trimmed)) return undefined;
  const value = Number(trimmed);
  return value >= min && value <= 65_535 ? value : undefined;
}

/**
 * The rules to save, or the first problem found, naming the rule. The
 * backend checks everything again; this only catches typos before a round
 * trip.
 */
export function rulesToSave(rows: readonly RuleRow[]): ForwardSpec[] | string {
  const specs: ForwardSpec[] = [];
  for (const [index, row] of rows.entries()) {
    const name = row.label.trim() || `rule ${index + 1}`;
    if (!row.label.trim()) return `Give ${name} a name`;
    const port = portOf(row.port, 0);
    if (port === undefined) return `${name}: the listening port must be 0 to 65535`;
    const bind = row.bind.trim();
    if (!bind) return `${name}: choose where it listens`;
    if (row.kind === "dynamic") {
      specs.push({
        id: row.id,
        label: row.label.trim(),
        kind: row.kind,
        bind,
        port,
        toHost: null,
        toPort: null,
        autoStart: row.autoStart,
      });
      continue;
    }
    const toHost = row.toHost.trim();
    if (!toHost) return `${name}: give the destination host`;
    const toPort = portOf(row.toPort, 1);
    if (toPort === undefined) return `${name}: the destination port must be 1 to 65535`;
    specs.push({
      id: row.id,
      label: row.label.trim(),
      kind: row.kind,
      bind,
      port,
      toHost,
      toPort,
      autoStart: row.autoStart,
    });
  }
  return specs;
}

/** True when the listener can be reached from other machines. */
export function opensToNetwork(bind: string): boolean {
  const value = bind.trim().toLowerCase();
  if (value === "loopback" || value === "localhost" || value === "::1") return false;
  return !/^(::ffff:)?127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(value);
}

function listener(bind: string, port: number, side: "here" | "server"): string {
  const where = bind === "loopback" ? (side === "here" ? "localhost" : "server's localhost") : bind;
  const shown = where.includes(":") ? `[${where}]` : where;
  return `${shown === "any" ? "*" : shown}:${port === 0 ? "auto" : port}`;
}

/** `localhost:15432 → db:5432`, as `ssh -L` would read it, for lists. */
export function describeRule(spec: ForwardSpec, port?: number | null): string {
  const listening = port ?? spec.port;
  const to = `${spec.toHost ?? ""}:${spec.toPort ?? ""}`;
  if (spec.kind === "local") {
    return `${listener(spec.bind, listening, "here")} → ${to} (from the server)`;
  }
  if (spec.kind === "remote") {
    return `${listener(spec.bind, listening, "server")} → ${to} (from here)`;
  }
  return `SOCKS proxy on ${listener(spec.bind, listening, "here")}`;
}

/** Narrows a `<select>` value to a rule kind. */
export function isKind(value: string): value is ForwardKindSpec {
  return value === "local" || value === "remote" || value === "dynamic";
}

/** A saved rule as the palette searches it. */
export interface RuleChoice {
  readonly hostId: string;
  readonly hostLabel: string;
  readonly ruleId: string;
  readonly ruleLabel: string;
  /** What the palette shows for it, either verb, lowercased once. */
  readonly words: string;
}

/** Every saved rule, for the palette. O(R), once per host list. */
export function ruleChoices(
  hosts: readonly Pick<HostView, "id" | "label" | "forwards">[],
): RuleChoice[] {
  return hosts.flatMap((host) =>
    host.forwards.map((rule) => ({
      hostId: host.id,
      hostLabel: host.label,
      ruleId: rule.id ?? "",
      ruleLabel: rule.label,
      words: `start stop forward: ${rule.label} (${host.label})`.toLowerCase(),
    })),
  );
}

/**
 * The first `limit` choices whose names hold every word typed, in list
 * order. O(R·w) for w words, stopping once `limit` are found.
 */
export function matchRules(
  choices: readonly RuleChoice[],
  search: string,
  limit: number,
): RuleChoice[] {
  const words = search.toLowerCase().split(/\s+/).filter(Boolean);
  const found: RuleChoice[] = [];
  for (const choice of choices) {
    if (found.length === limit) break;
    if (words.every((word) => choice.words.includes(word))) found.push(choice);
  }
  return found;
}
