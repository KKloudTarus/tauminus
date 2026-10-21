// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { formatEndpoint, parseDestination, parseJumps } from "./destination.ts";

describe("parseDestination", () => {
  it.each([
    ["db.example.com", { user: "", host: "db.example.com", port: 22 }],
    ["alice@db", { user: "alice", host: "db", port: 22 }],
    ["alice@db:2222", { user: "alice", host: "db", port: 2222 }],
    ["  ssh://alice@db:2200/ ", { user: "alice", host: "db", port: 2200 }],
    ["a@corp.example@db", { user: "a@corp.example", host: "db", port: 22 }],
    ["root@[2001:db8::1]:2222", { user: "root", host: "2001:db8::1", port: 2222 }],
    ["[::1]", { user: "", host: "::1", port: 22 }],
    ["fe80::1", { user: "", host: "fe80::1", port: 22 }],
    ["10.0.0.5:65535", { user: "", host: "10.0.0.5", port: 65_535 }],
  ])("accepts %j", (input, expected) => {
    expect(parseDestination(input)).toEqual({ ok: true, value: expected });
  });

  it.each([
    "",
    "alice@",
    "-oProxyCommand=x@db",
    "alice@db;rm",
    "alice@db:0",
    "alice@db:65536",
    "alice@db:22a",
    "alice@-db",
    "alice@db..example",
    "alice@[2001:db8::1",
    "alice@[2001:db8::1]2222",
    "alice@[db]:22",
    "al ice@db",
    `alice@${"a".repeat(64)}.com`,
  ])("rejects %j", (input) => {
    const parsed = parseDestination(input);
    expect(parsed.ok).toBe(false);
  });
});

describe("parseJumps", () => {
  it("splits on commas and skips blanks", () => {
    expect(parseJumps(" ops@bastion:2222, ,edge ")).toEqual({
      ok: true,
      value: [
        { user: "ops", host: "bastion", port: 2222 },
        { user: "", host: "edge", port: 22 },
      ],
    });
    expect(parseJumps("")).toEqual({ ok: true, value: [] });
  });

  it("reports the first bad hop", () => {
    expect(parseJumps("ok, bad;host")).toEqual({
      ok: false,
      error: '"bad;host" is not a valid host name',
    });
  });
});

describe("formatEndpoint", () => {
  it("brackets IPv6 and hides port 22", () => {
    expect(formatEndpoint({ user: "a", host: "db", port: 22 })).toBe("a@db");
    expect(formatEndpoint({ user: "", host: "::1", port: 2222 })).toBe("[::1]:2222");
  });

  it("round-trips through the parser", () => {
    for (const text of ["a@db", "a@db:2222", "[::1]:2222", "root@[2001:db8::1]"]) {
      const parsed = parseDestination(text);
      expect(parsed.ok && formatEndpoint(parsed.value)).toBe(text);
    }
  });
});
