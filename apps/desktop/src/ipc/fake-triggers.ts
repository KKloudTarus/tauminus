// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Highlight sets in memory for end-to-end tests. Only the `e2e` build loads
// it. Matching follows the backend's rules for the cases the tests use: an
// earlier rule wins text two match, whole-line rules are their own layer,
// and offsets count code points. Patterns are JavaScript's here, and the
// backend's linear-time engine in the app.

import type {
  FiredView,
  HitView,
  StyleSpec,
  TriggerSetView,
  TriggerView,
  TriggersClient,
} from "./triggers.ts";

/** What the backend's syntax refuses: lookaround and backreferences. */
const UNSUPPORTED = /\(\?<?[=!]|\\[1-9]/u;

function check(pattern: string): string | null {
  if (pattern === "") return "a pattern is needed";
  if (UNSUPPORTED.test(pattern)) return "the pattern is not valid: look-around is not supported";
  try {
    // oxlint-disable-next-line no-new -- compiling is the check
    new RegExp(pattern, "u");
    return null;
  } catch (error) {
    return `the pattern is not valid: ${String(error)}`;
  }
}

/** The sets every session gets, in their order. */
function defaultsOf(sets: readonly TriggerSetView[]): string[] {
  return sets
    .filter((s) => s.rank !== null && s.rank !== undefined)
    .toSorted((a, b) => (a.rank ?? 0) - (b.rank ?? 0))
    .map((s) => s.id);
}

/** Code points in `text` before UTF-16 offset `at`. */
function points(text: string, at: number): number {
  // Code points, as the backend counts.
  return text.slice(0, at).match(/./gsu)?.length ?? 0;
}

/**
 * `triggersOf` gives the set ids a saved host and its groups name, nearest
 * first.
 */
export function fakeTriggers(triggersOf: (host: string) => readonly string[]): TriggersClient {
  let sets: TriggerSetView[] = [];
  let next = 1;
  let revision = 0;
  const prepared = new Map<
    number,
    {
      revision: number;
      rules: { regex: RegExp; style: StyleSpec }[];
      triggers: { regex: RegExp; trigger: TriggerView }[];
    }
  >();
  let handles = 1;
  const sorted = () =>
    sets.toSorted((a, b) => a.name.toLowerCase().localeCompare(b.name.toLowerCase()));
  const changed = () => {
    revision += 1;
    return Promise.resolve(sorted());
  };
  return {
    list: () => Promise.resolve(sorted()),
    save: (input) => {
      for (const rule of input.rules) {
        const problem = check(rule.pattern);
        if (problem) return Promise.reject(new Error(problem));
        const { action } = rule;
        if (action.kind === "highlight" && !action.style.fg && !action.style.bg) {
          return Promise.reject(new Error("a highlight needs a text or a background colour"));
        }
        // oxlint-disable-next-line no-control-regex -- control characters are what is refused
        if (action.kind === "send" && /[\u0000-\u001f\u007f-\u009f]/u.test(action.text)) {
          return Promise.reject(new Error("text to send holds no control characters"));
        }
      }
      const id = input.id ?? `set-${next++}`;
      const old = sets.find((s) => s.id === id);
      const saved: TriggerSetView = {
        id,
        name: input.name.trim(),
        rules: input.rules,
        rank: old?.rank ?? null,
      };
      sets = [...sets.filter((s) => s.id !== id), saved];
      return changed();
    },
    remove: (id) => {
      sets = sets.filter((s) => s.id !== id);
      return changed();
    },
    saveDefaults: (ids) => {
      // Each set's place in the list, as the backend keeps it.
      sets = sets.map((s) => {
        const at = ids.indexOf(s.id);
        return { ...s, rank: at === -1 ? null : at };
      });
      return changed();
    },
    check: (pattern) => Promise.resolve(check(pattern)),
    prepare: (host) => {
      const named = [...(host ? triggersOf(host) : []), ...defaultsOf(sets)];
      const applied = [...new Set(named)]
        .flatMap((id) => sets.find((s) => s.id === id)?.rules ?? [])
        .filter((r) => r.enabled);
      const rules = applied.flatMap((r) =>
        r.action.kind === "highlight"
          ? [{ regex: new RegExp(r.pattern, "gu"), style: r.action.style }]
          : [],
      );
      const triggers = applied.flatMap((r) =>
        r.action.kind === "highlight"
          ? []
          : [{ regex: new RegExp(r.pattern, "u"), trigger: { name: r.name, action: r.action } }],
      );
      const handle = handles++;
      prepared.set(handle, { revision, rules, triggers });
      return Promise.resolve({
        handle,
        styles: rules.map((r) => r.style),
        triggers: triggers.map((t) => t.trigger),
      });
    },
    scan: (handle, lines) => {
      const entry = prepared.get(handle);
      if (!entry || entry.revision !== revision) return Promise.resolve(null);
      const hits: HitView[] = [];
      lines.forEach((text, line) => {
        const taken: [number, number][] = [];
        let lineTaken = false;
        const found: HitView[] = [];
        entry.rules.forEach(({ regex, style: spec }, style) => {
          regex.lastIndex = 0;
          if (spec.line) {
            if (!lineTaken && regex.test(text)) {
              lineTaken = true;
              found.push({ line, style, start: 0, end: points(text, text.length) });
            }
            return;
          }
          for (const match of text.matchAll(regex)) {
            const start = match.index;
            const end = start + match[0].length;
            if (start === end || taken.some(([s, e]) => start < e && s < end)) continue;
            taken.push([start, end]);
            found.push({ line, style, start: points(text, start), end: points(text, end) });
          }
        });
        found.sort((a, b) => a.start - b.start || b.end - a.end || a.style - b.style);
        hits.push(...found);
      });
      return Promise.resolve(hits);
    },
    fire: (handle, lines) => {
      const entry = prepared.get(handle);
      if (!entry || entry.revision !== revision) return Promise.resolve(null);
      const fired: FiredView[] = [];
      lines.forEach((text, line) => {
        entry.triggers.forEach(({ regex }, rule) => {
          const match = regex.exec(text);
          if (!match) return;
          const start = points(text, match.index);
          fired.push({
            line,
            rule,
            start,
            end: start + points(match[0], match[0].length),
            // The backend hides secrets of known shapes; enough of that here.
            text: text.replaceAll(/(token=)\S+/gu, "$1•••"),
          });
        });
      });
      return Promise.resolve(fired);
    },
    // The e2e build shows no system notifications: the app's notice does.
    notify: () => Promise.resolve(false),
  };
}
