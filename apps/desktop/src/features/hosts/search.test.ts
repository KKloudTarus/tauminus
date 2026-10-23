// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import type { HostView } from "@/ipc/index.ts";

import { searchHosts } from "./search.ts";

function host(label: string, address: string, tags: string[] = [], user = "me"): HostView {
  return {
    id: label,
    label,
    address,
    group: null,
    tags,
    settings: {},
    target: {
      destination: { user, host: address, port: 22 },
      jumps: [],
      route: { kind: "direct" },
      methods: null,
      identityFiles: [],
      identitiesOnly: false,
    },
    problem: null,
    guarded: false,
    forwardsAgent: false,
    forwards: [],
    startup: [],
    protocol: "ssh",
    joinsTmux: false,
  };
}

const hosts = [
  host("prod-db", "10.0.0.5", ["prod", "db"], "postgres"),
  host("prod-web", "web.example.com", ["prod"]),
  host("staging-db", "db.staging.internal", ["staging"]),
  host("Máy chủ", "vn.example.com"),
];
const labels = (query: string, group = "") =>
  searchHosts(hosts, query, () => group).map((m) => m.host.label);

function longest(i: number): HostView {
  return host(
    `${"x".repeat(120)}-${i}`,
    `${"a".repeat(60)}.example.com`,
    Array.from({ length: 32 }, (_, t) => `tag${t}${"y".repeat(26)}`),
  );
}

describe("searchHosts", () => {
  it("returns everything for an empty query", () => {
    expect(labels("  ")).toHaveLength(4);
  });

  it("needs every word to match some field", () => {
    expect(labels("prod db")).toEqual(["prod-db"]);
    expect(labels("db")).toEqual(["prod-db", "staging-db"]);
    expect(labels("nothing")).toEqual([]);
    // Two letters must be together: "db" is not "d…b" in prod-web.
    expect(labels("pwb")).toEqual(["prod-web"]);
  });

  it("matches address, user, tags and group name, ignoring case", () => {
    expect(labels("10.0.0")).toEqual(["prod-db"]);
    expect(labels("POSTGRES")).toEqual(["prod-db"]);
    expect(labels("staging")).toEqual(["staging-db"]);
    expect(labels("eu-west", "EU-West")).toHaveLength(4);
    expect(labels("eu-west", "")).toHaveLength(0);
    expect(labels("máy")).toEqual(["Máy chủ"]);
  });

  it("ranks a prefix above a word start above a substring above letters in order", () => {
    const ranked = searchHosts(
      [host("xdbsx", "a"), host("x-dbs", "b"), host("dbs-x", "c"), host("dxbxs", "d")],
      "dbs",
      () => "",
    ).map((m) => m.host.label);
    expect(ranked).toEqual(["dbs-x", "x-dbs", "xdbsx", "dxbxs"]);
  });

  it("finds a word start at any occurrence", () => {
    const ranked = searchHosts([host("adb-db", "a"), host("xadbx", "b")], "db", () => "").map(
      (m) => m.score,
    );
    expect(ranked).toEqual([3, 2]);
  });

  it("matches letters in order by code point", () => {
    expect(searchHosts([host("😀abcd", "a")], "😀bd", () => "")).toHaveLength(1);
  });

  it("stays fast when every field is at its longest", () => {
    const many = Array.from({ length: 10_000 }, (_, i) => longest(i));
    searchHosts(many, "warm", () => "group");
    const start = performance.now();
    searchHosts(many, "qqq", () => "group");
    expect(performance.now() - start).toBeLessThan(60);
  });

  it("handles ten thousand hosts within a frame", () => {
    const many = Array.from({ length: 10_000 }, (_, i) =>
      host(`host-${i}`, `10.${i >> 8}.${i & 255}.1`, ["fleet"]),
    );
    searchHosts(many, "host 1", () => "fleet");
    const start = performance.now();
    const found = searchHosts(many, "host 99", () => "fleet");
    const elapsed = performance.now() - start;
    expect(found.length).toBeGreaterThan(0);
    expect(elapsed).toBeLessThan(50);
  });
});
