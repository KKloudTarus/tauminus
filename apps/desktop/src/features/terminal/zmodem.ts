// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { ZmodemClient, ZmodemView } from "@/ipc/zmodem.ts";

/** Which way an offer's files go: `receive` from `sz`, `send` to `rz`. */
export type Direction = "receive" | "send";

/** The protocol an offer came in: Zmodem (`sz`, `rz`) or trzsz (`tsz`, `trz`). */
export type Protocol = "zmodem" | "trzsz";

/** What a pane shows of files its host sends or takes. */
export type ZmodemState =
  /**
   * `answered` once the user chose: one answer per offer. `shownAt` is
   * when it appeared: a click before {@link SETTLE} ms later was meant
   * for what was there before.
   */
  | {
      kind: "offered";
      offer: number;
      wants: Direction;
      protocol: Protocol;
      answered: boolean;
      shownAt: number;
    }
  | {
      /** Files come into `folder`, or, with none, go to the host. */
      kind: "moving";
      folder: string | null;
      name: string | null;
      bytes: number;
      size: number | null;
      files: number;
    }
  | {
      kind: "finished";
      folder: string | null;
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
export class ZmodemHub {
  readonly client: ZmodemClient;
  readonly #states = new Map<number, ZmodemState>();
  readonly #listeners = new Set<Listener>();

  constructor(client: ZmodemClient) {
    this.client = client;
    client
      .listen((event) => this.apply(event))
      .catch((error: unknown) => {
        console.error("file transfers are off", error);
      });
  }

  state(session: number | undefined): ZmodemState | undefined {
    return session === undefined ? undefined : this.#states.get(session);
  }

  subscribe(listener: Listener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /**
   * The user's answer to `session`'s offer, sent once: to `sz`, receive or
   * not; to `rz`, choose files (in the system's dialog) or not.
   */
  answer(session: number, accept: boolean): Promise<void> {
    const state = this.#states.get(session);
    if (state?.kind !== "offered" || state.answered || performance.now() - state.shownAt < SETTLE) {
      return Promise.resolve();
    }
    this.#states.set(session, { ...state, answered: true });
    this.#notify();
    const sent =
      state.wants === "send" && accept
        ? this.client.choose(session, state.offer).then(() => {})
        : this.client.receive(session, state.offer, accept && state.wants === "receive");
    return sent.catch((error: unknown) => {
      // Nothing went to the host: the offer is declined, and the user told.
      this.client.cancel(session).catch(() => {});
      this.apply({ kind: "failed", session, reason: messageOf(error) });
    });
  }

  /** Forgets what `session` showed: the pane closed it. */
  dismiss(session: number): void {
    if (this.#states.delete(session)) this.#notify();
  }

  apply(event: ZmodemView): void {
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

function advance(state: ZmodemState | undefined, event: ZmodemView): ZmodemState | undefined {
  switch (event.kind) {
    case "offered":
      return event.offer === null
        ? state
        : {
            kind: "offered",
            offer: event.offer,
            wants: event.wants,
            protocol: event.protocol,
            answered: false,
            shownAt: performance.now(),
          };
    case "declined":
      return undefined;
    case "started":
      return {
        kind: "moving",
        folder: event.folder,
        name: null,
        bytes: 0,
        size: null,
        files: 0,
      };
    case "file":
      return state?.kind === "moving"
        ? { ...state, name: event.name, bytes: 0, size: event.size, files: state.files + 1 }
        : state;
    case "progress":
      return state?.kind === "moving"
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
