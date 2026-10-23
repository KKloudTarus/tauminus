// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The only entry point to the backend. Lint forbids importing @tauri-apps/*
// anywhere else, so every IPC call goes through a typed port here.
import type { TextClipboard } from "./clipboard.ts";
import type { FilesClient } from "./files.ts";
import type { ForwardsClient } from "./forwards.ts";
import type { SnippetsClient } from "./snippets.ts";
import type { HistoryClient } from "./history.ts";
import type { VaultClient } from "./vault.ts";
import type { HostsClient } from "./hosts.ts";
import type { LinkOpener } from "./links.ts";
import type { SessionClient } from "./sessions.ts";

export {
  type GroupInput,
  type EnvSpec,
  type GroupView,
  type GuardFinding,
  type GuardSpec,
  type GuardVerdict,
  type HostInput,
  type HostView,
  type HostsClient,
  type HostsView,
  type ImportResult,
  type SettingsSpec,
  tauriHosts,
} from "./hosts.ts";
export { type TextClipboard, tauriClipboard } from "./clipboard.ts";
export {
  type ChangeSpec,
  type DroppedFiles,
  type EditView,
  type EntryView,
  type FilesClient,
  type Listing,
  type SftpHandle,
  type TransferView,
  tauriFiles,
} from "./files.ts";
export {
  type ForwardKindSpec,
  type ForwardSpec,
  type ForwardState,
  type ForwardsClient,
  tauriForwards,
} from "./forwards.ts";
export {
  type SkippedView,
  type SnippetInput,
  type SnippetView,
  type SnippetsClient,
  type StartupView,
  type VariableView,
  tauriSnippets,
} from "./snippets.ts";
export {
  type ExitFilterSpec,
  type HistoryClient,
  type HistoryHitSpec,
  type HistoryQuerySpec,
  type HistoryRecordSpec,
  tauriHistory,
} from "./history.ts";
export {
  type IdentityEdit,
  type IdentityView,
  type KeychainSpec,
  type KeyKindSpec,
  type VaultClient,
  type VaultStatusSpec,
  tauriVault,
} from "./vault.ts";
export { type LinkOpener, tauriLinks } from "./links.ts";
export {
  type AnswerSpec,
  type EndpointSpec,
  type ErrorCode,
  type GridSize,
  type PromptHandler,
  type PromptKind,
  type RouteSpec,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type SessionSpec,
  type SshTargetSpec,
  type TerminalSession,
  tauriSessions,
} from "./sessions.ts";

/**
 * The session client for this build. Vite replaces `import.meta.env.MODE` at
 * build time, so production bundles drop the fake shell entirely.
 */
export async function sessionClient(): Promise<SessionClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake.ts")).fakeSessions;
  }
  return (await import("./sessions.ts")).tauriSessions;
}

/** The hosts client for this build; the `e2e` build keeps hosts in memory. */
export async function hostsClient(): Promise<HostsClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-hosts.ts")).fakeHosts();
  }
  return (await import("./hosts.ts")).tauriHosts;
}

/** Opens links; the `e2e` build records them instead. */
export async function linkOpener(): Promise<LinkOpener> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-links.ts")).fakeLinks;
  }
  return (await import("./links.ts")).tauriLinks;
}

/** The clipboard; the `e2e` build, in a plain browser, uses the browser's. */
export async function textClipboard(): Promise<TextClipboard | undefined> {
  if (import.meta.env.MODE === "e2e") {
    return globalThis.navigator?.clipboard;
  }
  return (await import("./clipboard.ts")).tauriClipboard;
}

/** The vault; the `e2e` build keeps one in memory. */
export async function vaultClient(): Promise<VaultClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-vault.ts")).sharedFakeVault();
  }
  return (await import("./vault.ts")).tauriVault;
}

/** The command history; the `e2e` build keeps one in memory. */
export async function historyClient(): Promise<HistoryClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-history.ts")).fakeHistory();
  }
  return (await import("./history.ts")).tauriHistory;
}

/** Port forwarding; the `e2e` build only pretends, naming rules by `ruleLabel`. */
export async function forwardsClient(
  ruleLabel: (host: string, rule: string) => string,
): Promise<ForwardsClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-forwards.ts")).fakeForwards(ruleLabel);
  }
  return (await import("./forwards.ts")).tauriForwards;
}

/** Saved snippets; the `e2e` build keeps them in memory. */
export async function snippetsClient(
  startupOf: (host: string) => readonly string[],
): Promise<SnippetsClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-snippets.ts")).fakeSnippets(startupOf);
  }
  return (await import("./snippets.ts")).tauriSnippets;
}

/** Files on hosts and here; the `e2e` build keeps both sides in memory. */
export async function filesClient(): Promise<FilesClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-files.ts")).fakeFiles();
  }
  return (await import("./files.ts")).tauriFiles;
}
