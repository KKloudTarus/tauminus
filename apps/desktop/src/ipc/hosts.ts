// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type GroupInput,
  type EncodingSpec,
  type EnvSpec,
  type GroupView,
  type GuardFinding,
  type GuardSpec,
  type GuardVerdict,
  type HostInput,
  type HostView,
  type HostsView,
  type ImportResult,
  type SettingsSpec,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type {
  EncodingSpec,
  EnvSpec,
  GroupInput,
  GroupView,
  GuardFinding,
  GuardSpec,
  GuardVerdict,
  HostInput,
  HostView,
  HostsView,
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
  checkCommand: (hostId, lines) => unwrap(commands.guardCheck(hostId, lines)),
};
