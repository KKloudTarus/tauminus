// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Sync in memory for end-to-end tests. Only the `e2e` build loads it. One
// other device stands behind every target: joining with its passphrase,
// `correct horse battery`, brings its host `staging`, which waits for review.

import { sharedFakeVault } from "./fake-vault.ts";
import { SessionCommandError } from "./sessions.ts";
import type { ReviewView, SetupSpec, SyncClient, SyncStatusView, TargetInput } from "./sync.ts";

const THEIRS = "correct horse battery";

function fail(code: SessionCommandError["code"], message: string): never {
  throw new SessionCommandError({ code, message });
}

/** A remote without what stands before `@` in its authority; a bucket
 * by its name and service. */
function shown(target: TargetInput): string {
  if (target.kind === "s3") return `${target.s3?.bucket} at ${target.s3?.endpoint}`;
  const address = target.address ?? "";
  if (target.kind === "folder") return address;
  const [scheme, rest] = address.split("://", 2);
  if (rest === undefined) return address;
  const slash = rest.indexOf("/");
  const authority = slash < 0 ? rest : rest.slice(0, slash);
  const path = slash < 0 ? "" : rest.slice(slash);
  return `${scheme}://${authority.slice(authority.lastIndexOf("@") + 1)}${path}`;
}

const STAGING: ReviewView = {
  host: "staging",
  label: "staging",
  new: true,
  lines: [
    { level: 0, place: "staging", field: "address", value: "staging.internal" },
    { level: 0, place: "staging", field: "protocol", value: "Ssh" },
    { level: 0, place: "staging", field: "route", value: 'ProxyCommand("nc %h %p")' },
  ],
  startup: "cd /srv/app\r",
  fingerprint: "ab".repeat(32),
};

function open(): void {
  if (!sharedFakeVault().unlocked()) fail("vault_locked", "the vault is locked");
}

export function fakeSync(): SyncClient {
  let setup: SetupSpec = { kind: "off" };
  let last: SyncStatusView["last"] = null;
  let pending: ReviewView[] = [];
  const status = (): SyncStatusView => ({
    setup: sharedFakeVault().unlocked() ? setup : { kind: "locked" },
    running: false,
    last,
  });
  const start = (target: TargetInput) => {
    open();
    if (setup.kind === "on")
      fail("invalid_argument", "this device already syncs; stop syncing first");
    const where = target.kind === "s3" ? (target.s3?.endpoint ?? "") : (target.address ?? "");
    if (where.trim() === "") fail("connect_failed", "the sync target: no address");
  };
  return {
    status: async () => status(),
    async listen() {},
    async create(target, passphrase) {
      start(target);
      // Characters as the backend counts them: code points.
      if (Array.from(passphrase).length < 12) {
        fail("wrong_password", "the sync passphrase needs at least 12 characters");
      }
      setup = { kind: "on", through: target.kind, shown: shown(target) };
      last = null;
      return status();
    },
    async join(target, passphrase) {
      start(target);
      if (passphrase !== THEIRS) fail("wrong_password", "the passphrase does not open this sync");
      setup = { kind: "on", through: target.kind, shown: shown(target) };
      last = null;
      pending = [STAGING];
      return status();
    },
    async now() {
      open();
      if (setup.kind !== "on") fail("not_found", "sync is not set up on this device");
      last = {
        at: Date.now(),
        changed: true,
        error: null,
        clashes: pending.length
          ? [{ record: "staging", field: "port", kept: "2222", lost: "22" }]
          : [],
        repairs: [],
        notices: [],
      };
      return status();
    },
    async leave() {
      open();
      setup = { kind: "off" };
      last = null;
      return status();
    },
    reviews: async () => ({ reviews: pending, noted: pending.length }),
    async accept(seen) {
      const ok = seen.filter((s) =>
        pending.some((r) => r.host === s.host && r.fingerprint === s.fingerprint),
      );
      pending = pending.filter((r) => !ok.some((s) => s.host === r.host));
      return seen.filter((s) => !ok.includes(s)).map((s) => s.host);
    },
    chooseFolder: async () => "/home/me/Sync/tauminus",
  };
}
