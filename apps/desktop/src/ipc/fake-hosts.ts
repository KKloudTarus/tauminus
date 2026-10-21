// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Saved hosts in memory for end-to-end tests. Only the `e2e` build loads it.
// It applies group settings for user and port only; the Rust side owns the
// full rules and its tests cover them.

import type {
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

export function fakeHosts(): HostsClient {
  let groups: GroupView[] = [];
  let hosts: Omit<HostView, "target" | "problem">[] = [];
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
    };
    if (!host.label || !host.address) throw new Error("label and address are required");
    hosts = [...hosts.filter((h) => h.id !== host.id), host];
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
        });
        added += 1;
      }
      return { added, skipped, view: view() };
    },
  };
}
