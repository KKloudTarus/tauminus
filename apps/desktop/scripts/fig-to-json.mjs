// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Converts specs from withfig/autocomplete (MIT) into the static JSON the
// pane's completion reads (src/features/complete/spec.ts). Run by hand
// against a pinned checkout; the output is reviewed and committed, so no
// build runs upstream code.
//
//   node scripts/fig-to-json.mjs <autocomplete checkout> <commit>
//
// Each spec is evaluated in node:vm with its imports stubbed, which is no
// sandbox: run it only on a checkout you trust. Everything that runs code
// at completion time goes: generators, custom functions, scripts,
// templates' values. Only names, descriptions, the command tree, options,
// and fixed suggestions stay.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import path from "node:path";
import vm from "node:vm";

/** The commands kept: what server work reaches for, without the largest. */
export const COMMANDS = [
  "ansible",
  "apt",
  "cat",
  "chmod",
  "chown",
  "cp",
  "crontab",
  "curl",
  "df",
  "docker",
  "docker-compose",
  "du",
  "find",
  "git",
  "go",
  "grep",
  "head",
  "helm",
  "htop",
  "jq",
  "kill",
  "kubectl",
  "less",
  "ls",
  "make",
  "mount",
  "mv",
  "npm",
  "ping",
  "pkill",
  "pnpm",
  "ps",
  "python",
  "rm",
  "rsync",
  "scp",
  "sed",
  "ssh",
  "sudo",
  "sysctl",
  "systemctl",
  "tail",
  "tar",
  "terraform",
  "tmux",
  "top",
  "unzip",
  "wget",
  "yarn",
  "zip",
];

const MAX_DESCRIPTION = 120;
const MAX_NAME = 64;

const [checkout, commit] = process.argv.slice(2);
if (!checkout || !/^[0-9a-f]{40}$/u.test(commit ?? "")) {
  console.error("usage: node scripts/fig-to-json.mjs <checkout> <40-digit commit>");
  process.exit(2);
}
// The checkout is the commit named, as it was: UPSTREAM records it.
const git = (...args) =>
  execFileSync("git", ["-C", checkout, ...args], { encoding: "utf8" }).trim();
if (git("rev-parse", "HEAD") !== commit || git("status", "--porcelain") !== "") {
  console.error(`the checkout is not a clean ${commit}`);
  process.exit(2);
}
const out = path.join(import.meta.dirname, "..", "src", "features", "complete", "specs");
fs.mkdirSync(out, { recursive: true });

const names = (value) =>
  (Array.isArray(value) ? value : [value]).filter(
    (n) => typeof n === "string" && n !== "" && n.length <= MAX_NAME && !/\s/u.test(n),
  );

const description = (value) => {
  if (typeof value !== "string") return undefined;
  const text = value.replace(/\s+/gu, " ").trim();
  return text.length > MAX_DESCRIPTION
    ? `${text.slice(0, MAX_DESCRIPTION - 1)}…`
    : text || undefined;
};

/** `inOption`: an option's value, never a command line (upstream marks
 * `python -c`'s code as one). */
function arg(value, inOption = false) {
  if (!value || typeof value !== "object") return {};
  const a = {};
  const n = typeof value.name === "string" ? value.name : undefined;
  if (n) a.n = n.slice(0, MAX_NAME);
  const d = description(value.description);
  if (d) a.d = d;
  const suggestions = (Array.isArray(value.suggestions) ? value.suggestions : [value.suggestions])
    .flatMap((s) => (typeof s === "string" ? [{ name: s }] : s && typeof s === "object" ? [s] : []))
    .flatMap((s) => names(s.name).map((name) => ({ n: name, d: description(s.description) })))
    .map((s) => (s.d ? s : { n: s.n }));
  if (suggestions.length > 0) a.s = suggestions;
  if (value.isOptional) a.o = 1;
  if (value.isVariadic) a.v = 1;
  if (value.isCommand && !inOption) a.c = 1;
  const templates = Array.isArray(value.template) ? value.template : [value.template];
  if (templates.includes("filepaths")) a.t = "f";
  else if (templates.includes("folders")) a.t = "d";
  return a;
}

const args = (value, inOption = false) =>
  (Array.isArray(value) ? value : value ? [value] : []).map((a) => arg(a, inOption));

function option(value) {
  const o = { n: names(value.name) };
  const d = description(value.description);
  if (d) o.d = d;
  const a = args(value.args, true);
  if (a.length > 0) o.a = a;
  if (value.isPersistent) o.p = 1;
  if (value.isRepeatable) o.r = 1;
  return o;
}

function node(value, depth) {
  const n = { n: names(value.name) };
  const d = description(value.description);
  if (d) n.d = d;
  // `docker compose`: the rest is another command's spec, if one is kept.
  if (typeof value.loadSpec === "string" && COMMANDS.includes(value.loadSpec)) n.l = value.loadSpec;
  if (depth < 6) {
    const subcommands = (value.subcommands ?? [])
      .filter((s) => s && typeof s === "object")
      .map((s) => node(s, depth + 1))
      .filter((s) => s.n.length > 0);
    if (subcommands.length > 0) n.s = subcommands;
  }
  const options = (value.options ?? [])
    .filter((o) => o && typeof o === "object")
    .map(option)
    .filter((o) => o.n.length > 0);
  if (options.length > 0) n.o = options;
  const a = args(value.args);
  if (a.length > 0) n.a = a;
  return n;
}

// Any import becomes this: calling or reading it gives nothing.
const stub = new Proxy(function stubbed() {}, { get: () => stub, apply: () => ({}) });

let written = 0;
for (const command of COMMANDS) {
  const file = path.join(checkout, "src", `${command}.ts`);
  let js = stripTypeScriptTypes(fs.readFileSync(file, "utf8"));
  js = js.replace(/^import[\s\S]*?from\s+["'][^"']+["'];?/gmu, (line) =>
    [...line.matchAll(/([A-Za-z_$][\w$]*)(?=\s*[,}])|import\s+([A-Za-z_$][\w$]*)\s+from/gu)]
      .map((m) => `var ${m[1] ?? m[2]} = fig_stub;`)
      .join("\n"),
  );
  js = js
    .replace(/export\s+default\s+/gu, "fig_exports.default = ")
    .replace(/export\s+(const|function|let)\s+/gu, "$1 ");
  const context = {
    fig_stub: stub,
    fig_exports: {},
    console: { log() {}, warn() {} },
    process: { env: {} },
  };
  vm.runInNewContext(js, context, { timeout: 2000 });
  const spec = node(context.fig_exports.default, 0);
  spec.n = [command];
  fs.writeFileSync(path.join(out, `${command}.json`), `${JSON.stringify(spec)}\n`);
  written += 1;
}
fs.writeFileSync(
  path.join(out, "UPSTREAM"),
  `withfig/autocomplete ${commit}\nMIT License, Copyright (c) 2021 Hercules Labs Inc. (Fig)\n`,
);
console.error(`${written} specs written to ${out}`);
