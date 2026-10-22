// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// A vault in memory for end-to-end tests. Only the `e2e` build loads it. It
// keeps secrets to check the write-only rules; the real vault encrypts them.

import { SessionCommandError } from "./sessions.ts";
import type { IdentityView, VaultClient, VaultStatusSpec } from "./vault.ts";

interface Stored extends IdentityView {
  password: string | undefined;
  privateKey: string | undefined;
}

/** Write-only secrets: absent keeps the stored one, "" removes it. */
function keep(given: string | null | undefined, kept: string | undefined): string | undefined {
  return given == null ? kept : given === "" ? undefined : given;
}

function fail(code: SessionCommandError["code"], message: string): never {
  throw new SessionCommandError({ code, message });
}

/** The one vault the e2e build shares between the vault and SSH fakes. */
let shared: (VaultClient & { unlocked(): boolean }) | undefined;

export function sharedFakeVault(): VaultClient & { unlocked(): boolean } {
  shared ??= fakeVault();
  return shared;
}

export function fakeVault(): VaultClient & { unlocked(): boolean } {
  let password: string | undefined;
  let unlocked = false;
  let keychain = false;
  let idleMinutes = 15;
  let next = 1;
  let identities: Stored[] = [];
  const open = () => {
    if (!unlocked) fail("vault_locked", "the vault is locked");
  };
  const view = ({ password: _p, privateKey: _k, ...shown }: Stored): IdentityView => shown;
  return {
    unlocked: () => unlocked,
    async status(): Promise<VaultStatusSpec> {
      return {
        exists: password !== undefined,
        unlocked,
        keychain: keychain ? { kind: "on" } : { kind: "off" },
        idleMinutes,
      };
    },
    async create(chosen) {
      if (password !== undefined) fail("invalid_argument", "a vault already exists");
      if (Array.from(chosen).length < 8) {
        fail("invalid_argument", "the master password must be 8 to 1024 characters");
      }
      password = chosen;
      unlocked = true;
    },
    async unlock(given) {
      if (password === undefined) fail("not_found", "no vault exists yet");
      if (given !== password) fail("wrong_password", "the password is wrong");
      unlocked = true;
    },
    async unlockWithKeychain() {
      if (!keychain) fail("unavailable", "the OS keychain is unavailable: off");
      unlocked = true;
    },
    async lock() {
      unlocked = false;
    },
    async touch() {},
    async setIdle(minutes) {
      idleMinutes = minutes;
    },
    async changePassword(current, chosen) {
      open();
      if (current !== password) fail("wrong_password", "the password is wrong");
      password = chosen;
      return null;
    },
    async setKeychain(on, given) {
      if (on && given === undefined) fail("invalid_argument", "the master password is needed");
      if (on && given !== password) fail("wrong_password", "the password is wrong");
      open();
      keychain = on;
    },
    async identities() {
      open();
      return identities.map(view).toSorted((a, b) => a.name.localeCompare(b.name));
    },
    async saveIdentity(edit) {
      open();
      const old = identities.find((i) => i.id === edit.id);
      const stored: Stored = {
        id: edit.id ?? `identity${next++}`,
        name: edit.name.trim(),
        user: edit.user || null,
        password: keep(edit.password, old?.password),
        privateKey: keep(edit.privateKey, old?.privateKey),
        hasPassword: false,
        hasKey: false,
        publicKey: edit.privateKey ? "ssh-ed25519 AAAAfake" : (old?.publicKey ?? null),
      };
      if (!stored.name) fail("invalid_argument", "a name must be 1 to 128 characters");
      stored.hasPassword = stored.password !== undefined;
      stored.hasKey = stored.privateKey !== undefined;
      identities = [...identities.filter((i) => i.id !== stored.id), stored];
      return stored.id;
    },
    async deleteIdentity(id) {
      open();
      identities = identities.filter((i) => i.id !== id);
    },
  };
}
