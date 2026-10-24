// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { ReceiveClient, ReceiveView } from "@/ipc/receive.ts";

/** What a pane shows of files its host sends. */
export type ReceiveState =
  /**
   * `answered` once the user chose: one answer per offer. `shownAt` is
   * when it appeared: a click before {@link SETTLE} ms later was meant
   * for what was there before.
   */
  | { kind: "offered"; offer: number; answered: boolean; shownAt: number }
  | {
      kind: "receiving";
      folder: string;
      name: string | null;
      bytes: number;
      size: number | null;
      files: number;
    }
  | {
      kind: "finished";
      folder: string;
      files: string[];
      moreFiles: number;
      skipped: string[];
      moreSkipped: number;
    }
  | { kind: "failed"; reason: string };

type Listener = () => void;

/** Milliseconds an offer shows before an answer counts. */
export const SETTLE = 400;

/**
 * Every session's transfer state, from the backend's one event channel.
 * Each state is a new object when it changes, so a pane's snapshot stays
 * the same between events. O(1) per event, plus one call per listener.
 */
export class ReceiveHub {
  readonly client: ReceiveClient;
  readonly #states = new Map<number, ReceiveState>();
  readonly #listeners = new Set<Listener>();

  constructor(client: ReceiveClient) {
    this.client = client;
    client
      .listen((event) => this.apply(event))
      .catch((error: unknown) => {
        console.error("file transfers are off", error);
      });
  }

  state(session: number | undefined): ReceiveState | undefined {
    return session === undefined ? undefined : this.#states.get(session);
  }

  subscribe(listener: Listener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** The user's answer to `session`'s offer, sent once. */
  answer(session: number, accept: boolean): Promise<void> {
    const state = this.#states.get(session);
    if (state?.kind !== "offered" || state.answered || performance.now() - state.shownAt < SETTLE) {
      return Promise.resolve();
    }
    this.#states.set(session, { ...state, answered: true });
    this.#notify();
    return this.client.answer(session, state.offer, accept).catch((error: unknown) => {
      // Nothing went to the host: the offer is declined, and the user told.
      this.client.cancel(session).catch(() => {});
      this.apply({ kind: "failed", session, reason: messageOf(error) });
    });
  }

  /** Forgets what `session` showed: the pane closed it. */
  dismiss(session: number): void {
    if (this.#states.delete(session)) this.#notify();
  }

  apply(event: ReceiveView): void {
    const before = this.#states.get(event.session);
    const next = advance(before, event);
    if (next === before) return;
    if (next === undefined) this.#states.delete(event.session);
    else this.#states.set(event.session, next);
    this.#notify();
  }

  #notify(): void {
    for (const listener of this.#listeners) listener();
  }
}

function advance(state: ReceiveState | undefined, event: ReceiveView): ReceiveState | undefined {
  switch (event.kind) {
    case "offered":
      return event.offer === null
        ? state
        : { kind: "offered", offer: event.offer, answered: false, shownAt: performance.now() };
    case "declined":
      return undefined;
    case "started":
      return {
        kind: "receiving",
        folder: event.folder,
        name: null,
        bytes: 0,
        size: null,
        files: 0,
      };
    case "file":
      return state?.kind === "receiving"
        ? { ...state, name: event.name, bytes: 0, size: event.size, files: state.files + 1 }
        : state;
    case "progress":
      return state?.kind === "receiving"
        ? { ...state, bytes: event.bytes ?? state.bytes, size: event.size }
        : state;
    case "finished":
      return {
        kind: "finished",
        folder: event.folder,
        files: event.files,
        moreFiles: event.moreFiles ?? 0,
        skipped: event.skipped,
        moreSkipped: event.moreSkipped ?? 0,
      };
    case "failed":
      return { kind: "failed", reason: event.reason };
    default:
      return state;
  }
}

/** Bytes for people: `512 B`, `1.5 MB`. */
export function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${units[unit]}`;
}
