// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Snippets in memory for end-to-end tests. Only the `e2e` build loads it.
// Placeholders follow the backend's rules for the cases the tests use:
// `{{name}}` and `{{name:default}}`.

import type { SnippetView, SnippetsClient, VariableView } from "./snippets.ts";

const PLACEHOLDER = /\{\{([\p{L}_][\p{L}\p{N}_.-]{0,63})(?::([^}\n]*))?\}\}/gu;

function variables(body: string): VariableView[] {
  const seen = new Map<string, VariableView>();
  for (const [, name = "", fallback] of body.matchAll(PLACEHOLDER)) {
    if (!seen.has(name)) seen.set(name, { name, default: fallback ?? null });
  }
  return [...seen.values()];
}

function fill(body: string, values: Record<string, string>): string {
  return body.replaceAll(PLACEHOLDER, (_whole, name: string, fallback: string | undefined) => {
    const given = values[name];
    const value = given?.trim() ? given : fallback;
    if (value === undefined) throw new Error(`no value for {{${name}}}`);
    return value;
  });
}

function command(body: string, values: Record<string, string>): string {
  return fill(body, values)
    .replace(/[\r\n]+$/u, "")
    .split(/\r?\n/u)
    .map((line) => `${line}\r`)
    .join("");
}

/** `startupOf` gives the startup snippet ids a saved host lists. */
export function fakeSnippets(startupOf: (host: string) => readonly string[]): SnippetsClient {
  let snippets: SnippetView[] = [];
  let next = 1;
  const sorted = () =>
    snippets.toSorted(
      (a, b) =>
        (a.folder ?? "").localeCompare(b.folder ?? "") ||
        a.name.toLowerCase().localeCompare(b.name.toLowerCase()),
    );
  return {
    list: () => Promise.resolve(sorted()),
    save: (input) => {
      if (!input.name.trim()) return Promise.reject(new Error("a snippet needs a name"));
      if (!input.body.trim()) return Promise.reject(new Error("a snippet needs a command"));
      const id = input.id ?? `snippet-${next++}`;
      const folder = input.folder?.trim() ? input.folder.trim() : null;
      const saved: SnippetView = {
        id,
        name: input.name.trim(),
        folder,
        body: input.body,
        variables: variables(input.body),
      };
      snippets = [...snippets.filter((s) => s.id !== id), saved];
      return Promise.resolve(sorted());
    },
    remove: (id) => {
      snippets = snippets.filter((s) => s.id !== id);
      return Promise.resolve(sorted());
    },
    command: async (id, values) => {
      const snippet = snippets.find((s) => s.id === id);
      if (!snippet) throw new Error(`no snippet ${id}`);
      return command(snippet.body, values);
    },
    startup: async (host) => {
      let text = "";
      const skipped = [];
      for (const id of startupOf(host)) {
        const snippet = snippets.find((s) => s.id === id);
        try {
          if (!snippet) throw new Error(`no snippet ${id}`);
          text += command(snippet.body, {});
        } catch (error) {
          skipped.push({ id, reason: error instanceof Error ? error.message : String(error) });
        }
      }
      return { text, skipped };
    },
  };
}
