// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { MAX_SUGGESTIONS, suggest } from "./match.ts";
import type { SpecNode } from "./spec.ts";
import { COMMANDS, loadSpec } from "./specs.ts";
import { tokenize } from "./tokenize.ts";

const GIT: SpecNode = {
  n: ["git"],
  o: [{ n: ["-C"], a: [{ n: "path" }], p: 1 }, { n: ["--version"] }],
  s: [
    {
      n: ["checkout", "co"],
      d: "Switch branches",
      o: [
        { n: ["-b"], a: [{ n: "branch" }] },
        { n: ["--force", "-f"], d: "Throw away changes" },
      ],
      a: [{ n: "branch", o: 1 }],
    },
    { n: ["commit"], o: [{ n: ["-m", "--message"], a: [{ n: "message" }], r: 1 }] },
    { n: ["config"], a: [{ s: [{ n: "user.name" }, { n: "core.editor", d: "Editor" }] }] },
  ],
};
const TAR: SpecNode = {
  n: ["tar"],
  o: [{ n: ["-v"] }],
  s: [{ n: ["-x", "--extract"], o: [{ n: ["-f"], a: [{ n: "file" }] }, { n: ["-z"] }] }],
};
const DOCKER: SpecNode = {
  n: ["docker"],
  s: [{ n: ["compose"], l: "docker-compose" }, { n: ["ps"] }],
};
const COMPOSE: SpecNode = { n: ["docker-compose"], s: [{ n: ["up"] }, { n: ["down"] }] };
const PY: SpecNode = { n: ["py"], o: [{ n: ["-c"], a: [{ c: 1 }] }, { n: ["--tabs="] }] };
const WGET: SpecNode = {
  n: ["wget"],
  o: [{ n: ["--execute=COMMAND"] }, { n: ["--chown=USER:GROUP"] }],
};
const SUDO: SpecNode = { n: ["sudo"], o: [{ n: ["-u"], a: [{ n: "user" }] }], a: [{ c: 1, v: 1 }] };
const specs: Record<string, SpecNode> = {
  git: GIT,
  sudo: SUDO,
  tar: TAR,
  docker: DOCKER,
  "docker-compose": COMPOSE,
  py: PY,
  wget: WGET,
};
const load = (command: string) => Promise.resolve(specs[command]);
const names = async (text: string) =>
  (await suggest(tokenize(text), ["git", "grep", "sudo"], load)).map((s) => s.name);

describe("tokenize", () => {
  it("splits words, keeps quoted spaces, and starts over after a separator", () => {
    expect(tokenize("git commit -m 'a b' ")).toEqual({
      words: ["git", "commit", "-m", "a b"],
      current: "",
      quoted: false,
    });
    expect(tokenize("cd /tmp && git ch").words).toEqual(["git"]);
    expect(tokenize("ls | gr").current).toBe("gr");
    expect(tokenize("ls | gr").words).toEqual([]);
    expect(tokenize("echo $(git st").words).toEqual(["git"]);
    expect(tokenize("a;b").words).toEqual([]);
    expect(tokenize("git commit -m 'unfinished").quoted).toBe(true);
    expect(tokenize("cat my\\ fi").quoted).toBe(true);
    expect(tokenize("make 2>&1 -").words).toEqual(["make", "2>&1"]);
    expect(tokenize("ls &>/dev/null x").words).toEqual(["ls", "&>/dev/null"]);
  });
});

describe("suggest", () => {
  it("offers commands, subcommands by any name, and their options", async () => {
    expect(await names("g")).toEqual(["git", "grep"]);
    expect(await names("git c")).toEqual(["checkout", "commit", "config"]);
    expect(await names("git co")).toEqual(["commit", "config"]);
    expect(await names("git checkout --")).toEqual(["--force"]);
    expect(await names("git checkout -")).toEqual(["-b", "-f", "-C"]);
  });

  it("follows options' values, persistent options, and fixed values", async () => {
    expect(await names("git -C /srv c")).toEqual(["checkout", "commit", "config"]);
    expect(await names("git checkout -b ")).toEqual([]);
    expect(await names("git config ")).toEqual(["user.name", "core.editor"]);
    const config = await suggest(tokenize("git config co"), [], load);
    expect(config).toEqual([{ name: "core.editor", kind: "value", description: "Editor" }]);
  });

  it("does not offer a used option again, unless it repeats", async () => {
    expect(await names("git checkout --force -")).toEqual(["-b", "-C"]);
    expect(await names("git commit -m x -")).toEqual(["-m", "-C"]);
  });

  it("starts over at the command sudo runs", async () => {
    expect(await names("sudo -u root gi")).toEqual(["git"]);
    expect(await names("sudo git co")).toEqual(["commit", "config"]);
    expect(await names("FOO=1 git ch")).toEqual(["checkout"]);
  });

  it("offers nothing for an unknown command or a word that needs quoting", async () => {
    expect(await names("frobnicate --")).toEqual([]);
    expect(await names("git commit -m 'a")).toEqual([]);
    expect(await names("git checkout")).toEqual([]);
  });
});

describe("suggest, on the shapes of real specs", () => {
  it("enters subcommands named like options, as tar's modes", async () => {
    expect(await names("tar -")).toEqual(["-x", "-v"]);
    expect(await names("tar -x -")).toEqual(["-f", "-z"]);
  });

  it("goes on in another spec where a subcommand is one", async () => {
    expect(await names("docker compose u")).toEqual(["up"]);
    expect(await names("docker p")).toEqual(["ps"]);
  });

  it("never offers commands as an option's value, nor placeholders", async () => {
    expect(await names("py -c ")).toEqual([]);
    expect(await names("py --t")).toEqual([]);
    expect(await names("wget --")).toEqual([]);
  });

  it("skips redirections, ends options at --, and drops assignments after sudo", async () => {
    expect(await names("git >/dev/null c")).toEqual(["checkout", "commit", "config"]);
    expect(await names("git > out c")).toEqual(["checkout", "commit", "config"]);
    expect(await names("git > ")).toEqual([]);
    expect(await names("git &> out c")).toEqual(["checkout", "commit", "config"]);
    expect(await names("git checkout -- -")).toEqual([]);
    expect(await names("git -- c")).toEqual([]);
    expect(await names("sudo FOO=1 git ch")).toEqual(["checkout"]);
  });
});

describe("the shipped specs", () => {
  it("load lazily, hold no code, and fit the cap", async () => {
    expect(COMMANDS).toContain("git");
    expect(COMMANDS.length).toBeGreaterThanOrEqual(40);
    const git = await loadSpec("git");
    expect(JSON.stringify(git)).not.toMatch(/"generators"|"script"|"custom"/u);
    expect(await loadSpec("not-a-command")).toBeUndefined();
    const all = await suggest(tokenize("docker run --"), COMMANDS, loadSpec);
    expect(all.length).toBeGreaterThan(10);
    expect(all.length).toBeLessThanOrEqual(MAX_SUGGESTIONS);
    expect(all.every((s) => s.name.startsWith("--"))).toBe(true);
    expect(
      (await suggest(tokenize("systemctl res"), COMMANDS, loadSpec)).map((s) => s.name),
    ).toContain("restart");
  });
});
