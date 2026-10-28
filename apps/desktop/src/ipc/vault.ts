// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type IdentityEdit,
  type IdentityView,
  type KeychainSpec,
  type KeyKindSpec,
  type SecretRefSpec,
  type SecretRefsSpec,
  type VaultStatusSpec,
  commands,
} from "./bindings.ts";
import { type PromptHandler, SessionCommandError, promptChannel } from "./sessions.ts";

export type {
  IdentityEdit,
  IdentityView,
  KeychainSpec,
  KeyKindSpec,
  SecretRefSpec,
  SecretRefsSpec,
  VaultStatusSpec,
};

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
  /** Makes keychain unlock ask the OS to confirm the user is present, or
   * stop asking; the OS asks once now, either way. */
  setPresence(on: boolean): Promise<void>;
  identities(): Promise<IdentityView[]>;
  saveIdentity(identity: IdentityEdit): Promise<string>;
  deleteIdentity(id: string): Promise<void>;
  /** Adds an identity's public key to `authorized_keys` on a saved host,
   * asking `onPrompt` what a connect to it would ask. */
  installKey(identity: string, host: string, onPrompt: PromptHandler): Promise<void>;
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
  setPresence: (on) => done(commands.vaultSetPresence(on)),
  identities: () => unwrap(commands.identitiesList()),
  saveIdentity: (identity) => unwrap(commands.identitySave(identity)),
  deleteIdentity: (id) => done(commands.identityDelete(id)),
  installKey: (identity, host, onPrompt) => {
    const prompts = promptChannel(onPrompt);
    return done(commands.identityInstall(identity, host, prompts));
  },
};
