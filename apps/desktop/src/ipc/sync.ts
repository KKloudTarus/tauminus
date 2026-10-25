// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import {
  type ClashView,
  type ExposedView,
  type ReportView,
  type ReviewView,
  type ReviewsView,
  type SeenInput,
  type SetupSpec,
  type SyncStatusView,
  type TargetInput,
  type TargetKind,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type {
  ClashView,
  ExposedView,
  ReportView,
  ReviewView,
  ReviewsView,
  SeenInput,
  SetupSpec,
  SyncStatusView,
  TargetInput,
  TargetKind,
};

/**
 * End-to-end encrypted sync of hosts and snippets (ADR 0030). The passphrase
 * goes in and is not kept; the key and the target's address stay in the
 * vault, and a remote comes back without its token. A host another device
 * changed waits for review: connecting to it fails with `needs_review`
 * until `accept` gets the fingerprint the user was shown. Reviews come a
 * page at a time.
 */
export interface SyncClient {
  status(): Promise<SyncStatusView>;
  /** Where each scheduled run's status goes; called once. */
  listen(handler: (status: SyncStatusView) => void): Promise<void>;
  create(target: TargetInput, passphrase: string): Promise<SyncStatusView>;
  join(target: TargetInput, passphrase: string): Promise<SyncStatusView>;
  now(): Promise<SyncStatusView>;
  leave(): Promise<SyncStatusView>;
  reviews(): Promise<ReviewsView>;
  /** Resolves to the hosts that changed since they were shown. */
  accept(seen: SeenInput[]): Promise<string[]>;
  /** The system's folder dialog; `null` when nothing was chosen. */
  chooseFolder(): Promise<string | null>;
}

type Result<T> = Promise<
  | { status: "ok"; data: T }
  | { status: "error"; error: { code: SessionCommandError["code"]; message: string } }
>;

async function unwrap<T>(result: Result<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") throw new SessionCommandError(outcome.error);
  return outcome.data;
}

export const tauriSync: SyncClient = {
  status: () => unwrap(commands.syncStatus()),
  async listen(handler) {
    const channel = new Channel<SyncStatusView>();
    channel.onmessage = handler;
    await commands.syncListen(channel);
  },
  create: (target, passphrase) => unwrap(commands.syncCreate(target, passphrase)),
  join: (target, passphrase) => unwrap(commands.syncJoin(target, passphrase)),
  now: () => unwrap(commands.syncNow()),
  leave: () => unwrap(commands.syncLeave()),
  reviews: () => unwrap(commands.syncReviews()),
  accept: (seen) => unwrap(commands.syncReviewAccept(seen)),
  chooseFolder: () => unwrap(commands.syncChooseFolder()),
};
