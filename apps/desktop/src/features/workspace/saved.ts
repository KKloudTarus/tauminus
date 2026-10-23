// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { EndpointSpec } from "@/ipc/index.ts";

import type { Direction, Template } from "./layout.ts";
import { MAX_PANES } from "./workspace.ts";

const STORAGE_KEY = "tauminus.workspaces";
/** Most workspaces kept. */
export const MAX_WORKSPACES = 50;
const MAX_NAME = 128;
/** Longest id kept: ids the app makes are far shorter. */
const MAX_ID = 128;

/**
 * What a saved pane opens. A saved host is kept by id and looked up when
 * it opens, so an edit to the host applies. A typed-in connection keeps
 * its endpoint and jump hosts only: a proxy command read back from
 * storage would run whatever was written there.
 */
export type SavedSpec =
  | { readonly kind: "local" }
  | { readonly kind: "host"; readonly hostId: string }
  | {
      readonly kind: "ssh";
      readonly destination: EndpointSpec;
      readonly jumps: readonly EndpointSpec[];
    };

export interface SavedWorkspace {
  readonly id: string;
  readonly name: string;
  readonly tabs: readonly Template<SavedSpec>[];
}

/**
 * Saved workspaces, in this window's local storage beside the theme and
 * key bindings: layout and host ids, no secrets. What is read back is
 * checked field by field and anything malformed is dropped. O(P) per
 * workspace.
 */
export class SavedWorkspaces {
  readonly #storage: Storage | undefined;
  readonly #listeners = new Set<() => void>();
  #all: readonly SavedWorkspace[];

  constructor(storage: Storage | undefined) {
    this.#storage = storage;
    this.#all = read(storage);
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): readonly SavedWorkspace[] => this.#all;

  find(id: string): SavedWorkspace | undefined {
    return this.#all.find((w) => w.id === id);
  }

  /** Saves `tabs` under `name`, replacing one of that name. False past the limits. */
  save(name: string, tabs: readonly Template<SavedSpec>[]): boolean {
    const trimmed = name.trim();
    if (!trimmed || trimmed.length > MAX_NAME || tabs.length === 0) return false;
    const same = this.#all.find((w) => w.name === trimmed);
    if (!same && this.#all.length >= MAX_WORKSPACES) return false;
    const saved: SavedWorkspace = { id: same?.id ?? newId(), name: trimmed, tabs };
    this.#set(same ? this.#all.map((w) => (w === same ? saved : w)) : [...this.#all, saved]);
    return true;
  }

  remove(id: string): void {
    this.#set(this.#all.filter((w) => w.id !== id));
  }

  #set(all: readonly SavedWorkspace[]): void {
    this.#all = all;
    try {
      this.#storage?.setItem(STORAGE_KEY, JSON.stringify(all));
    } catch (error) {
      console.warn("could not save workspaces", error);
    }
    for (const listener of this.#listeners) listener();
  }
}

function newId(): string {
  return `ws-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

function read(storage: Storage | undefined): SavedWorkspace[] {
  let raw: unknown;
  try {
    raw = JSON.parse(storage?.getItem(STORAGE_KEY) ?? "[]");
  } catch {
    return [];
  }
  if (!Array.isArray(raw)) return [];
  const all: SavedWorkspace[] = [];
  const ids = new Set<string>();
  for (const item of raw.slice(0, MAX_WORKSPACES)) {
    const workspace = savedWorkspace(item);
    if (workspace && !ids.has(workspace.id)) {
      ids.add(workspace.id);
      all.push(workspace);
    }
  }
  return all;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function savedWorkspace(value: unknown): SavedWorkspace | undefined {
  if (!isObject(value)) return undefined;
  const { id, name, tabs } = value;
  if (typeof id !== "string" || typeof name !== "string" || !Array.isArray(tabs)) return undefined;
  if (!id || id.length > MAX_ID) return undefined;
  if (!name.trim() || name.length > MAX_NAME) return undefined;
  const budget = { panes: MAX_PANES };
  const parsed: Template<SavedSpec>[] = [];
  for (const tab of tabs) {
    const template = savedTemplate(tab, budget, 0);
    if (!template) return undefined;
    parsed.push(template);
  }
  return parsed.length > 0 ? { id, name, tabs: parsed } : undefined;
}

/** Depth and pane counts are bounded, so a crafted value cannot recurse or grow without end. */
function savedTemplate(
  value: unknown,
  budget: { panes: number },
  depth: number,
): Template<SavedSpec> | undefined {
  if (!isObject(value) || depth > 16) return undefined;
  const { kind, spec, direction, sizes, children } = value;
  if (kind === "pane") {
    budget.panes -= 1;
    const parsed = savedSpec(spec);
    return budget.panes >= 0 && parsed ? { kind: "pane", spec: parsed } : undefined;
  }
  if (kind !== "split" || !isDirection(direction) || !Array.isArray(children)) return undefined;
  if (!Array.isArray(sizes)) return undefined;
  const shares: number[] = [];
  for (const size of sizes) {
    if (typeof size !== "number") return undefined;
    shares.push(size);
  }
  const parsed: Template<SavedSpec>[] = [];
  for (const child of children) {
    const template = savedTemplate(child, budget, depth + 1);
    if (!template) return undefined;
    parsed.push(template);
  }
  return { kind: "split", direction, sizes: shares, children: parsed };
}

function isDirection(value: unknown): value is Direction {
  return value === "row" || value === "column";
}

function savedSpec(value: unknown): SavedSpec | undefined {
  if (!isObject(value)) return undefined;
  const { kind, hostId, destination, jumps } = value;
  if (kind === "local") return { kind: "local" };
  if (kind === "host" && typeof hostId === "string" && hostId.length <= MAX_ID) {
    return { kind: "host", hostId };
  }
  if (kind !== "ssh" || !Array.isArray(jumps) || jumps.length > 8) return undefined;
  const to = endpoint(destination);
  const via: EndpointSpec[] = [];
  for (const jump of jumps) {
    const parsed = endpoint(jump);
    if (!parsed) return undefined;
    via.push(parsed);
  }
  return to ? { kind: "ssh", destination: to, jumps: via } : undefined;
}

function endpoint(value: unknown): EndpointSpec | undefined {
  if (!isObject(value)) return undefined;
  const { user, host, port } = value;
  if (typeof user !== "string" || typeof host !== "string" || typeof port !== "number") {
    return undefined;
  }
  // The limits the backend holds them to.
  if (user.length > 64 || !host || host.length > 253) return undefined;
  return Number.isInteger(port) && port > 0 && port < 65536 ? { user, host, port } : undefined;
}

/**
 * Where a workspace connects to directly, as `user@host:port via jump`:
 * shown before it opens, since what storage holds decides where it goes.
 */
export function directConnections(workspace: SavedWorkspace): string[] {
  const out: string[] = [];
  const visit = (template: Template<SavedSpec>) => {
    if (template.kind === "split") {
      for (const child of template.children) visit(child);
    } else if (template.spec.kind === "ssh") {
      const via = template.spec.jumps.map(show).join(", ");
      out.push(
        via ? `${show(template.spec.destination)} via ${via}` : show(template.spec.destination),
      );
    }
  };
  for (const tab of workspace.tabs) visit(tab);
  return out;
}

function show(at: EndpointSpec): string {
  return `${at.user ? `${at.user}@` : ""}${at.host}:${at.port}`;
}
