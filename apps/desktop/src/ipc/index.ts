// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The only entry point to the backend. Lint forbids importing @tauri-apps/*
// anywhere else, so every IPC call goes through a typed port here.
import type { AiClient } from "./ai.ts";
import type { TextClipboard } from "./clipboard.ts";
import type { FilesClient } from "./files.ts";
import type { ForwardsClient } from "./forwards.ts";
import type { SerialClient } from "./serial.ts";
import type { SnippetsClient } from "./snippets.ts";
import type { TriggersClient } from "./triggers.ts";
import type { HistoryClient } from "./history.ts";
import type { SyncClient } from "./sync.ts";
import type { VaultClient } from "./vault.ts";
import type { UpdatesClient } from "./updates.ts";
import type { HostsClient } from "./hosts.ts";
import type { LinkOpener } from "./links.ts";
import type { ZmodemClient } from "./zmodem.ts";
import type { RecordingsClient } from "./recordings.ts";
import type { SessionClient, TmuxClient } from "./sessions.ts";

export {
  type GroupInput,
  type EncodingSpec,
  type EnvSpec,
  type ProtocolSpec,
  type GroupView,
  type GuardFinding,
  type GuardSpec,
  type GuardVerdict,
  type HostInput,
  type HostView,
  type HostsClient,
  type HostsView,
  type ImportKind,
  type ImportResult,
  type SettingsSpec,
  type CloudAsk,
  type CloudPreview,
  type CloudResult,
  tauriHosts,
} from "./hosts.ts";
export {
  type AiClient,
  type AiPreviewView,
  type AiReplyView,
  type AiSettingsSpec,
  type AiSettingsView,
  tauriAi,
} from "./ai.ts";
export { type TextClipboard, tauriClipboard } from "./clipboard.ts";
export type { UpdatesClient, UpdateView } from "./updates.ts";
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
  type FlowSpec,
  type ParitySpec,
  type SerialClient,
  type SerialPortView,
  type SerialPortsView,
  type SerialSpec,
  describePort,
  tauriSerial,
} from "./serial.ts";
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
  type ActionSpec,
  type FiredView,
  type HitView,
  type PreparedView,
  type RuleSpec,
  type StyleSpec,
  type TriggerSetInput,
  type TriggerSetView,
  type TriggerView,
  type TriggersClient,
  tauriTriggers,
} from "./triggers.ts";
export {
  type ExitFilterSpec,
  type HistoryClient,
  type HistoryHitSpec,
  type HistoryQuerySpec,
  type HistoryRecordSpec,
  tauriHistory,
} from "./history.ts";
export {
  type ClashView,
  type ExposedView,
  type ReportView,
  type ReviewView,
  type ReviewsView,
  type S3Input,
  type SeenInput,
  type SetupSpec,
  type SyncClient,
  type SyncStatusView,
  type TargetInput,
  type TargetKind,
  tauriSync,
} from "./sync.ts";
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
  type ActiveRecording,
  type BookmarkSpec,
  type RecordingView,
  type RecordingsClient,
  tauriRecordings,
} from "./recordings.ts";
export {
  type AnswerSpec,
  type EndpointSpec,
  type ErrorCode,
  type GridSize,
  type PromptHandler,
  type LocalShellView,
  type LocalShellsView,
  type PromptKind,
  type RouteSpec,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type SessionSpec,
  type SshTargetSpec,
  type TabWindowView,
  type TerminalSession,
  type TmuxClient,
  type TmuxEvents,
  type TopologyView,
  tauriSessions,
  tauriTmux,
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

/** New versions; the `e2e` build offers one only when a test asks. */
export async function updatesClient(storage: Storage | undefined): Promise<UpdatesClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-updates.ts")).fakeUpdates(storage);
  }
  return (await import("./updates.ts")).tauriUpdates;
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

/** Sync; the `e2e` build pretends, with one other device behind it. */
export async function syncClient(): Promise<SyncClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-sync.ts")).fakeSync();
  }
  return (await import("./sync.ts")).tauriSync;
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

/** Serial ports; the `e2e` build lists two that do not exist. */
export async function serialClient(): Promise<SerialClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-serial.ts")).fakeSerial();
  }
  return (await import("./serial.ts")).tauriSerial;
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

/**
 * Highlight sets; the `e2e` build keeps them in memory. `triggersOf` gives
 * the sets a saved host and its groups name, nearest first.
 */
export async function triggersClient(
  triggersOf: (host: string) => readonly string[],
): Promise<TriggersClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-triggers.ts")).fakeTriggers(triggersOf);
  }
  return (await import("./triggers.ts")).tauriTriggers;
}

/** Files on hosts and here; the `e2e` build keeps both sides in memory. */
export async function filesClient(): Promise<FilesClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-files.ts")).fakeFiles();
  }
  return (await import("./files.ts")).tauriFiles;
}

/**
 * Session recordings; the `e2e` build keeps them in memory, sealed to the
 * fake vault, and offers `__tauminusSeedRecording(title, cast)` and
 * `__tauminusCutRecordings()` to tests.
 */
export async function recordingsClient(): Promise<RecordingsClient> {
  if (import.meta.env.MODE === "e2e") {
    const vault = (await import("./fake-vault.ts")).sharedFakeVault();
    const fake = (await import("./fake-recordings.ts")).fakeRecordings(() => !vault.unlocked());
    Reflect.set(globalThis, "__tauminusSeedRecording", (title: string, cast: string) =>
      fake.seed(title, cast),
    );
    Reflect.set(globalThis, "__tauminusCutRecordings", () => fake.cut());
    Reflect.set(globalThis, "__tauminusHourLongCast", async (bytes: number) =>
      (await import("./fake-recordings.ts")).fakeHourLongCast(bytes),
    );
    return fake;
  }
  return (await import("./recordings.ts")).tauriRecordings;
}

/** The AI assistant; the `e2e` build answers from a script. */
export async function aiClient(): Promise<AiClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake-ai.ts")).fakeAi();
  }
  return (await import("./ai.ts")).tauriAi;
}

/** Files hosts send with `sz`; the `e2e` build's fake shell offers them. */
export async function zmodemClient(): Promise<ZmodemClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake.ts")).fakeZmodem;
  }
  return (await import("./zmodem.ts")).tauriZmodem;
}

/** tmux sessions as tabs; the `e2e` build's fake shows two windows. */
export async function tmuxClient(): Promise<TmuxClient> {
  if (import.meta.env.MODE === "e2e") {
    return (await import("./fake.ts")).fakeTmux;
  }
  return (await import("./sessions.ts")).tauriTmux;
}
