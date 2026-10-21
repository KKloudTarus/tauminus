// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type GroupInput,
  type GroupView,
  type HostInput,
  type HostView,
  type HostsView,
  type ImportResult,
  type SettingsSpec,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { GroupInput, GroupView, HostInput, HostView, HostsView, ImportResult, SettingsSpec };

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
};
