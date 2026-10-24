// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { SpecArg, SpecNode, SpecOption } from "./spec.ts";
import type { TypedLine } from "./tokenize.ts";

export interface Suggestion {
  name: string;
  description?: string;
  kind: "command" | "subcommand" | "option" | "value";
}

/** A command's spec, loaded when first asked for; `undefined` if none. */
export type SpecLoader = (command: string) => Promise<SpecNode | undefined>;

/** Most suggestions offered at once. */
export const MAX_SUGGESTIONS = 50;

/** Names that would need quoting, or would mean something to the shell. */
const UNSAFE = /[\s;&|<>(){}$`'"\\*?[\]!#~]/u;

/**
 * What may follow `line`: commands, subcommands, options, and fixed values
 * of `commands`' specs, starting with the current word. Nothing for a word
 * that needs quoting, or a command without a spec. Commands that run another
 * (`sudo`, `yarn`) start over at it, and subcommands kept as another spec
 * (`docker compose`) go on in it. O(w·f) for w words and fan-out f.
 */
export async function suggest(
  line: TypedLine,
  commands: readonly string[],
  load: SpecLoader,
): Promise<Suggestion[]> {
  if (line.quoted) return [];
  let words = [...line.words];
  for (let depth = 0; depth < 4; depth += 1) {
    // `FOO=1 cmd`: assignments before a command are not it.
    while (words.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*=/u.test(words[0] ?? "")) {
      words = words.slice(1);
    }
    const [command, ...rest] = words;
    if (command === undefined) {
      return pick(
        commands.map((name) => entry(name, "command", undefined)),
        line.current,
      );
    }
    // One after another: the next command is only known from this spec.
    // oxlint-disable-next-line no-await-in-loop
    const spec = await load(command.slice(command.lastIndexOf("/") + 1));
    if (!spec) return [];
    const walked = walk(spec, rest);
    if (walked.kind === "command") {
      words = walked.words;
      continue;
    }
    if (walked.kind === "spec") {
      words = [walked.spec, ...walked.words];
      continue;
    }
    return pick(offers(walked, line.current, commands), line.current);
  }
  return [];
}

interface Position {
  kind: "position";
  node: SpecNode;
  /** Options of ancestors that subcommands take too. */
  inherited: SpecOption[];
  used: Set<SpecOption>;
  /** The positional argument next, by index. */
  argIndex: number;
  /** An option before the cursor wants a value. */
  value: SpecArg | undefined;
  /** Past `--`: no options. */
  optionsDone: boolean;
}

/** A word that redirects (`>out`, `2>&1`, `<in`), which is no argument. */
const REDIRECT = /^(?:\d*[<>]|&>)/u;

/** Where the words before the cursor lead, or another command they start. */
function walk(
  spec: SpecNode,
  words: readonly string[],
):
  | Position
  | { kind: "command"; words: string[] }
  | { kind: "spec"; spec: string; words: string[] } {
  let node = spec;
  const inherited: SpecOption[] = [];
  let used = new Set<SpecOption>();
  let argIndex = 0;
  let value: SpecArg | undefined;
  let optionsDone = false;
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i] ?? "";
    if (value) {
      value = undefined;
      continue;
    }
    if (word === "--") {
      optionsDone = true;
      continue;
    }
    if (REDIRECT.test(word)) {
      // `> file`: the next word is the file.
      if (/[<>]$/u.test(word)) value = {};
      continue;
    }
    // `tar -x`: some subcommands are named like options.
    const sub =
      argIndex === 0 && !optionsDone ? node.s?.find((s) => s.n.includes(word)) : undefined;
    if (!sub && !optionsDone && word.length > 1 && word.startsWith("-")) {
      const name = word.split("=", 1)[0] ?? word;
      const option = [...(node.o ?? []), ...inherited].find((o) => o.n.includes(name));
      if (option) {
        used.add(option);
        const wanted = option.a?.[0];
        if (wanted && !wanted.o && !word.includes("=")) value = wanted;
      }
      continue;
    }
    if (sub) {
      if (sub.l) return { kind: "spec", spec: sub.l, words: words.slice(i + 1) };
      for (const option of node.o ?? []) if (option.p) inherited.push(option);
      node = sub;
      used = new Set();
      continue;
    }
    if (argAt(node, argIndex)?.c) return { kind: "command", words: words.slice(i) };
    argIndex += 1;
  }
  return { kind: "position", node, inherited, used, argIndex, value, optionsDone };
}

function argAt(node: SpecNode, index: number): SpecArg | undefined {
  const args = node.a ?? [];
  const last = args.at(-1);
  return args[index] ?? (last?.v ? last : undefined);
}

function offers(at: Position, current: string, commands: readonly string[]): Suggestion[] {
  // An option's value is never a command line, whatever a spec says.
  if (at.value) return at.value.c ? [] : values(at.value, commands);
  if (current.startsWith("-") && !at.optionsDone) {
    const long = current.startsWith("--");
    const named =
      at.argIndex === 0
        ? (at.node.s ?? []).flatMap((s) => {
            const name = s.n.find((n) => n.startsWith(current));
            return name ? [entry(name, "subcommand", s.d)] : [];
          })
        : [];
    const options = [...(at.node.o ?? []), ...at.inherited]
      .filter((o) => o.r || !at.used.has(o))
      .flatMap((o) => {
        // One entry per option: its long name for `--`, else its first.
        const name =
          o.n.find((n) => n.startsWith(current) && n.startsWith("--") === long) ??
          o.n.find((n) => n.startsWith(current));
        return name ? [entry(name, "option", o.d)] : [];
      });
    return [...named, ...options];
  }
  const subcommands =
    at.argIndex === 0 && !at.optionsDone
      ? (at.node.s ?? []).map((s) =>
          entry(s.n.find((n) => n.startsWith(current)) ?? s.n[0] ?? "", "subcommand", s.d),
        )
      : [];
  const arg = argAt(at.node, at.argIndex);
  return [...subcommands, ...(arg ? values(arg, commands) : [])];
}

function values(arg: SpecArg, commands: readonly string[]): Suggestion[] {
  if (arg.c) return commands.map((name) => entry(name, "command", undefined));
  return (arg.s ?? []).map((s) => entry(s.n, "value", s.d));
}

function entry(
  name: string,
  kind: Suggestion["kind"],
  description: string | undefined,
): Suggestion {
  const suggestion: Suggestion = { name, kind };
  if (description) suggestion.description = description;
  return suggestion;
}

/** Those starting with `current`, safe to type, once each, at most the cap. */
function pick(all: Suggestion[], current: string): Suggestion[] {
  const seen = new Set<string>();
  const out: Suggestion[] = [];
  for (const s of all) {
    if (out.length >= MAX_SUGGESTIONS) break;
    if (!s.name.startsWith(current) || s.name === current || UNSAFE.test(s.name)) continue;
    // `--execute=COMMAND`: a placeholder after `=`, which is no value.
    if (/=(?:[A-Z_:-]*)$/u.test(s.name)) continue;
    if (seen.has(s.name)) continue;
    seen.add(s.name);
    out.push(s);
  }
  return out;
}
