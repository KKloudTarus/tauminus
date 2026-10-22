// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type IdentityEdit,
  type IdentityView,
  type KeychainSpec,
  type VaultStatusSpec,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { IdentityEdit, IdentityView, KeychainSpec, VaultStatusSpec };

/** The vault. Secrets go in; only whether one is stored comes back. */
export interface VaultClient {
  status(): Promise<VaultStatusSpec>;
  create(password: string): Promise<void>;
  unlock(password: string): Promise<void>;
  unlockWithKeychain(): Promise<void>;
  lock(): Promise<void>;
  touch(): Promise<void>;
  setIdle(minutes: number): Promise<void>;
  /** Resolves to why the keychain was turned off, if it refused its new
   * key; the password changed either way. */
  changePassword(current: string, next: string): Promise<string | null>;
  /** Turning it on takes the master password again. */
  setKeychain(on: boolean, password?: string): Promise<void>;
  identities(): Promise<IdentityView[]>;
  saveIdentity(identity: IdentityEdit): Promise<string>;
  deleteIdentity(id: string): Promise<void>;
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

async function done(result: Result<null>): Promise<void> {
  await unwrap(result);
}

export const tauriVault: VaultClient = {
  status: () => unwrap(commands.vaultStatus()),
  create: (password) => done(commands.vaultCreate(password)),
  unlock: (password) => done(commands.vaultUnlock(password)),
  unlockWithKeychain: () => done(commands.vaultUnlockKeychain()),
  lock: () => done(commands.vaultLock()),
  touch: () => done(commands.vaultTouch()),
  setIdle: (minutes) => done(commands.vaultSetIdle(minutes)),
  changePassword: (current, next) => unwrap(commands.vaultChangePassword(current, next)),
  setKeychain: (on, password) => done(commands.vaultSetKeychain(on, password ?? null)),
  identities: () => unwrap(commands.identitiesList()),
  saveIdentity: (identity) => unwrap(commands.identitySave(identity)),
  deleteIdentity: (id) => done(commands.identityDelete(id)),
};
