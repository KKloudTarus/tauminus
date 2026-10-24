// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Saved hosts in memory for end-to-end tests. Only the `e2e` build loads it.
// It applies group settings for user and port only; the Rust side owns the
// full rules and its tests cover them.

import type {
  GuardFinding,
  GuardVerdict,
  GroupInput,
  GroupView,
  HostInput,
  HostView,
  HostsClient,
  HostsView,
} from "./hosts.ts";

const ME = "me";

/** Hosts `importConfig` adds, named after the fake SSH hosts in `fake.ts`. */
const CONFIG = [
  { label: "prompt", address: "prompt.test", user: "alice" },
  { label: "kbd", address: "kbd.test", user: "bob" },
  { label: "changed", address: "changed.test", user: null },
];

/** The address of each fake host, for the fake sessions. */
const addresses = new Map<string, string>();

/** A fake saved host's address, or `undefined`. */
export function fakeHostAddress(id: string): string | undefined {
  return addresses.get(id);
}

export function fakeHosts(): HostsClient {
  let groups: GroupView[] = [];
  let hosts: Omit<HostView, "target" | "problem" | "guarded" | "forwardsAgent">[] = [];
  let next = 1;
  const id = () => `id${next++}`;

  const nearest = <K extends "user" | "port">(
    start: string | null,
    own: HostView["settings"][K],
    key: K,
  ) => {
    if (own != null) return own;
    let group = groups.find((g) => g.id === start);
    while (group) {
      const value = group.settings[key];
      if (value != null) return value;
      const parent = group.parent;
      group = groups.find((g) => g.id === parent);
    }
    return undefined;
  };

  /** The nearest level that turns the guard on or off; else the `prod` tag. */
  const guarded = (host: (typeof hosts)[number]) => {
    let enabled = host.settings.guard?.enabled;
    let group = groups.find((g) => g.id === host.group);
    while (enabled == null && group) {
      enabled = group.settings.guard?.enabled;
      const parent = group.parent;
      group = groups.find((g) => g.id === parent);
    }
    return enabled ?? host.tags.includes("prod");
  };

  const forwardsAgent = (host: (typeof hosts)[number]) => {
    let forward = host.settings.forwardAgent;
    let group = groups.find((g) => g.id === host.group);
    while (forward == null && group) {
      forward = group.settings.forwardAgent;
      const parent = group.parent;
      group = groups.find((g) => g.id === parent);
    }
    return forward ?? false;
  };

  const view = (): HostsView => ({
    groups: structuredClone(groups),
    hosts: hosts.map((h) => ({
      ...structuredClone(h),
      target: {
        destination: {
          user: nearest(h.group, h.settings.user, "user") ?? ME,
          host: h.address,
          port: nearest(h.group, h.settings.port, "port") ?? 22,
        },
        jumps: [],
        route: { kind: "direct" },
        methods: null,
        identityFiles: [],
        identitiesOnly: false,
      },
      problem: null,
      guarded: guarded(h),
      forwardsAgent: forwardsAgent(h),
    })),
  });

  const saveHost = (input: HostInput) => {
    const host = {
      id: input.id ?? id(),
      label: input.label.trim(),
      address: input.address.trim(),
      group: input.group ?? null,
      tags: [...new Set((input.tags ?? []).map((t) => t.toLowerCase()))].toSorted(),
      settings: input.settings ?? {},
      // Absent keeps the saved rules, as the backend does.
      forwards: (input.forwards ?? hosts.find((h) => h.id === input.id)?.forwards ?? []).map(
        (rule) => Object.assign({}, rule, { id: rule.id ?? id() }),
      ),
      startup: input.startup ?? hosts.find((h) => h.id === input.id)?.startup ?? [],
      protocol: input.protocol ?? "ssh",
      // The fake reads only the host's own setting, not its groups'.
      joinsTmux: (input.protocol ?? "ssh") === "ssh" && Boolean(input.settings?.tmux),
      tmuxTabs:
        (input.protocol ?? "ssh") === "ssh" &&
        Boolean(input.settings?.tmux) &&
        input.settings?.tmuxTabs === true,
    };
    if (!host.label || !host.address) throw new Error("label and address are required");
    hosts = [...hosts.filter((h) => h.id !== host.id), host];
    addresses.set(host.id, host.address);
  };

  return {
    list: async () => view(),
    saveHost: async (input) => {
      saveHost(input);
      return view();
    },
    deleteHost: async (hostId) => {
      hosts = hosts.filter((h) => h.id !== hostId);
      return view();
    },
    saveGroup: async (input: GroupInput) => {
      const group = {
        id: input.id ?? id(),
        name: input.name.trim(),
        parent: input.parent ?? null,
        settings: input.settings ?? {},
      };
      groups = [...groups.filter((g) => g.id !== group.id), group];
      return view();
    },
    deleteGroup: async (groupId) => {
      const parent = groups.find((g) => g.id === groupId)?.parent ?? null;
      groups = groups.filter((g) => g.id !== groupId);
      for (const group of groups) if (group.parent === groupId) group.parent = parent;
      for (const host of hosts) if (host.group === groupId) host.group = parent;
      return view();
    },
    configAliases: async () => CONFIG.length,
    checkCommand: async (hostId, lines) => {
      const host = hosts.find((h) => h.id === hostId);
      if (!host) throw new Error(`no host ${hostId}`);
      if (!guarded(host)) return { findings: [], incomplete: false };
      const verdicts = lines.map(fakeVerdict);
      const findings = verdicts.flatMap((v) => v.findings);
      const unique = findings.filter(
        (f, at) => findings.findIndex((g) => g.rule === f.rule && g.command === f.command) === at,
      );
      return { findings: unique, incomplete: verdicts[0]?.incomplete ?? false };
    },
    importConfig: async () => {
      let group = groups.find((g) => g.name === "SSH config");
      if (!group) {
        group = { id: id(), name: "SSH config", parent: null, settings: {} };
        groups = [...groups, group];
      }
      let added = 0;
      const skipped = [];
      for (const entry of CONFIG) {
        if (hosts.some((h) => h.label === entry.label)) {
          skipped.push({ alias: entry.label, reason: "already saved" });
          continue;
        }
        saveHost({
          label: entry.label,
          address: entry.address,
          group: group.id,
          settings: { user: entry.user },
          forwards: [],
        });
        added += 1;
      }
      return { added, skipped, view: view() };
    },
  };
}

/**
 * A stand-in for the backend's guard, enough to drive the dialog: flags
 * `rm -r…`, `reboot`, and `drop table`; a trailing `\` or an odd number of
 * single quotes makes the line incomplete.
 */
function fakeVerdict(line: string): GuardVerdict {
  const joined = line.replaceAll("\\\n", "");
  const findings: GuardFinding[] = [];
  const rm = /\brm\s+-\w*r\w*(\s+\S+)*/.exec(joined);
  if (rm)
    findings.push({
      rule: "recursive-delete",
      reason: "deletes files recursively",
      command: rm[0],
    });
  if (/\breboot\b/.test(joined)) {
    findings.push({
      rule: "power-off",
      reason: "shuts down or reboots the machine",
      command: "reboot",
    });
  }
  if (/\bdrop\s+table\b/i.test(joined)) {
    findings.push({ rule: "drop-data", reason: "drops a database object", command: joined.trim() });
  }
  const quotes = (joined.match(/'/g) ?? []).length;
  return { findings, incomplete: joined.endsWith("\\") || quotes % 2 === 1 };
}
