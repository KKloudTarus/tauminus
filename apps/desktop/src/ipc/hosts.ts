// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type CloudAsk,
  type CloudPreview,
  type CloudResult,
  type GroupInput,
  type EncodingSpec,
  type ProtocolSpec,
  type EnvSpec,
  type GroupView,
  type GuardFinding,
  type GuardSpec,
  type GuardVerdict,
  type HostInput,
  type HostView,
  type HostsView,
  type ImportKind,
  type ImportResult,
  type SettingsSpec,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type {
  CloudAsk,
  CloudPreview,
  CloudResult,
  EncodingSpec,
  ProtocolSpec,
  EnvSpec,
  GroupInput,
  GroupView,
  GuardFinding,
  GuardSpec,
  GuardVerdict,
  HostInput,
  HostView,
  HostsView,
  ImportKind,
  ImportResult,
  SettingsSpec,
};

/** Saved hosts. Every change returns the whole list. */
export interface HostsClient {
  list(): Promise<HostsView>;
  saveHost(host: HostInput): Promise<HostsView>;
  deleteHost(id: string): Promise<HostsView>;
  saveGroup(group: GroupInput): Promise<HostsView>;
  deleteGroup(id: string): Promise<HostsView>;
  /** How many hosts `~/.ssh/config` names. */
  configAliases(): Promise<number>;
  importConfig(): Promise<ImportResult>;
  /**
   * Hosts from another client: from the file or folder the user picks in
   * the system's dialog, or PuTTY's own sessions. `null` when nothing was
   * picked.
   */
  importFrom(kind: ImportKind): Promise<ImportResult | null>;
  /**
   * Lists a cloud account through its provider's CLI and shows what an
   * import would do; nothing is saved.
   */
  cloudPreview(ask: CloudAsk): Promise<CloudPreview>;
  /** Imports what the preview with `token` showed. */
  cloudImport(token: number): Promise<CloudResult>;
  /**
   * Checks the readings of a command line about to run on a saved host
   * against its prod guard. `incomplete` comes from the first.
   */
  checkCommand(hostId: string, lines: string[]): Promise<GuardVerdict>;
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

export const tauriHosts: HostsClient = {
  list: () => unwrap(commands.hostsList()),
  saveHost: (host) => unwrap(commands.hostSave(host)),
  deleteHost: (id) => unwrap(commands.hostDelete(id)),
  saveGroup: (group) => unwrap(commands.groupSave(group)),
  deleteGroup: (id) => unwrap(commands.groupDelete(id)),
  configAliases: () => unwrap(commands.sshConfigAliases()),
  importConfig: () => unwrap(commands.hostsImportSshConfig()),
  importFrom: (kind) => unwrap(commands.hostsImportFrom(kind)),
  cloudPreview: (ask) => unwrap(commands.cloudPreview(ask)),
  cloudImport: (token) => unwrap(commands.cloudImport(token)),
  checkCommand: (hostId, lines) => unwrap(commands.guardCheck(hostId, lines)),
};
