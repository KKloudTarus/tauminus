// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import type { ForwardSpec, HostView } from "@/ipc/index.ts";

import {
  describeRule,
  matchRules,
  newRow,
  opensToNetwork,
  ruleChoices,
  ruleRow,
  rulesToSave,
} from "./rules.ts";

const local: ForwardSpec = {
  id: "pg",
  label: "postgres",
  kind: "local",
  bind: "loopback",
  port: 15432,
  toHost: "db.internal",
  toPort: 5432,
  autoStart: true,
};

describe("rulesToSave", () => {
  it("round-trips a saved rule through its row", () => {
    expect(rulesToSave([ruleRow(local)])).toEqual([local]);
  });

  it("drops the destination of a dynamic rule", () => {
    const row = { ...newRow("dynamic"), label: "socks", toHost: "left over", toPort: "1" };
    expect(rulesToSave([row])).toEqual([
      {
        id: null,
        label: "socks",
        kind: "dynamic",
        bind: "loopback",
        port: 1080,
        toHost: null,
        toPort: null,
        autoStart: false,
      },
    ]);
  });

  it("names the rule that has a problem", () => {
    const good = ruleRow(local);
    expect(rulesToSave([good, { ...good, label: " " }])).toBe("Give rule 2 a name");
    expect(rulesToSave([{ ...good, port: "70000" }])).toMatch(/^postgres: the listening port/);
    expect(rulesToSave([{ ...good, port: "-1" }])).toMatch(/listening port/);
    expect(rulesToSave([{ ...good, toPort: "0" }])).toMatch(/destination port/);
    expect(rulesToSave([{ ...good, toHost: "" }])).toMatch(/destination host/);
    expect(rulesToSave([{ ...good, port: "0" }])).not.toBeTypeOf("string");
  });
});

describe("opensToNetwork", () => {
  it("tells loopback apart from everything else", () => {
    const here = ["loopback", "localhost", "127.0.0.1", "127.5.0.9", "::1", "::ffff:127.0.0.1"];
    expect(here.filter(opensToNetwork)).toEqual([]);
    const open = ["any", "0.0.0.0", "::", "192.168.1.5", "128.0.0.1", "::ffff:10.0.0.1"];
    expect(open.filter(opensToNetwork)).toEqual(open);
  });
});

describe("describeRule", () => {
  it("reads like the ssh flag it stands for", () => {
    expect(describeRule(local)).toBe("localhost:15432 → db.internal:5432 (from the server)");
    expect(describeRule({ ...local, kind: "remote", bind: "any", port: 0 }, 40001)).toBe(
      "*:40001 → db.internal:5432 (from here)",
    );
    expect(describeRule({ ...local, kind: "remote", port: 0 })).toBe(
      "server's localhost:auto → db.internal:5432 (from here)",
    );
    expect(describeRule({ ...local, kind: "dynamic", bind: "::1", port: 1080 })).toBe(
      "SOCKS proxy on [::1]:1080",
    );
  });
});

function host(id: string, rules: string[]): Pick<HostView, "id" | "label" | "forwards"> {
  return {
    id,
    label: id.toUpperCase(),
    forwards: rules.map((label, i) => Object.assign({}, local, { id: `${id}${i}`, label })),
  };
}

describe("matchRules", () => {
  const choices = ruleChoices([host("db", ["postgres", "redis"]), host("web", ["socks"])]);

  it("needs every word, in any order, across rule and host names", () => {
    expect(matchRules(choices, "web socks", 50).map((c) => c.ruleId)).toEqual(["web0"]);
    expect(matchRules(choices, "DB", 50).map((c) => c.ruleLabel)).toEqual(["postgres", "redis"]);
    expect(matchRules(choices, "Start forward: postgres", 50).map((c) => c.ruleId)).toEqual([
      "db0",
    ]);
    expect(matchRules(choices, "stop redis", 50)).toHaveLength(1);
    expect(matchRules(choices, "mysql", 50)).toHaveLength(0);
    expect(matchRules(choices, "forward", 50)).toHaveLength(3);
  });

  it("stops at the limit", () => {
    const many = ruleChoices([
      host(
        "h",
        Array.from({ length: 500 }, (_, i) => `r${i}`),
      ),
    ]);
    expect(matchRules(many, "", 50)).toHaveLength(50);
  });
});
