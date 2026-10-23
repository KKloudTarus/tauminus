// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * App shortcuts, kept apart from the shell's keys.
 *
 * Terminals pass most keys to the shell, so app shortcuts use modifiers the
 * shell rarely needs: Ctrl+Shift on Linux and Windows (as Windows Terminal and
 * GNOME Terminal do) and Cmd on macOS. Plain Ctrl+K stays with the shell,
 * where it kills to the end of the line.
 *
 * A chord is normalized to one string ("ctrl+shift+t"), so lookup is a single
 * `Map` access: O(1) per keystroke. Letters follow the printed key on Latin
 * layouts and fall back to the physical key when an input method such as
 * Telex hides the letter (see `keyOf`).
 */

export type Action =
  | "palette.open"
  | "tab.new"
  | "tab.close"
  | "tab.next"
  | "tab.previous"
  | "tab.moveLeft"
  | "tab.moveRight"
  | "pane.splitRight"
  | "pane.splitDown"
  | "pane.close"
  | "pane.focusLeft"
  | "pane.focusRight"
  | "pane.focusUp"
  | "pane.focusDown"
  | "terminal.copy"
  | "terminal.paste"
  | "terminal.clear"
  | "theme.cycle"
  | "ssh.connect"
  | "hosts.toggle"
  | "hosts.import"
  | "vault.open"
  | "vault.lock"
  | "history.open"
  | "forwards.open"
  | "files.open"
  | "snippets.open"
  | "broadcast.toggle"
  | "broadcast.tab"
  | "broadcast.off"
  | "workspaces.open"
  | "serial.open";

export const ACTIONS: readonly Action[] = [
  "palette.open",
  "tab.new",
  "tab.close",
  "tab.next",
  "tab.previous",
  "tab.moveLeft",
  "tab.moveRight",
  "pane.splitRight",
  "pane.splitDown",
  "pane.close",
  "pane.focusLeft",
  "pane.focusRight",
  "pane.focusUp",
  "pane.focusDown",
  "terminal.copy",
  "terminal.paste",
  "terminal.clear",
  "theme.cycle",
  "ssh.connect",
  "hosts.toggle",
  "hosts.import",
  "vault.open",
  "vault.lock",
  "history.open",
  "forwards.open",
  "files.open",
  "snippets.open",
  "broadcast.toggle",
  "broadcast.tab",
  "broadcast.off",
  "workspaces.open",
  "serial.open",
];

export type Platform = "mac" | "other";

/** Bindings per action. An action may have several chords. */
export type Bindings = Partial<Record<Action, readonly string[]>>;

const OTHER: Bindings = {
  "palette.open": ["ctrl+shift+p"],
  "tab.new": ["ctrl+shift+t"],
  "tab.close": ["ctrl+shift+w"],
  "tab.next": ["ctrl+tab", "ctrl+pagedown"],
  "tab.previous": ["ctrl+shift+tab", "ctrl+pageup"],
  "tab.moveLeft": ["ctrl+shift+pageup"],
  "tab.moveRight": ["ctrl+shift+pagedown"],
  "pane.splitRight": ["ctrl+shift+d"],
  "pane.splitDown": ["ctrl+shift+e"],
  "pane.close": ["ctrl+shift+x"],
  "pane.focusLeft": ["alt+arrowleft"],
  "pane.focusRight": ["alt+arrowright"],
  "pane.focusUp": ["alt+arrowup"],
  "pane.focusDown": ["alt+arrowdown"],
  "terminal.copy": ["ctrl+shift+c"],
  "terminal.paste": ["ctrl+shift+v"],
  "terminal.clear": ["ctrl+shift+k"],
  "theme.cycle": [],
  "ssh.connect": ["ctrl+shift+o"],
  "hosts.toggle": ["ctrl+shift+h"],
  "hosts.import": [],
  "vault.open": [],
  "vault.lock": [],
  "history.open": ["ctrl+shift+r"],
  "forwards.open": [],
  "files.open": [],
  "snippets.open": [],
  "broadcast.toggle": [],
  "broadcast.tab": [],
  "broadcast.off": [],
  "workspaces.open": [],
  "serial.open": [],
};

const MAC: Bindings = {
  // Cmd+K clears the scrollback in Terminal.app and iTerm2; keep that habit.
  "palette.open": ["meta+shift+p"],
  "tab.new": ["meta+t"],
  "tab.close": ["meta+w"],
  "tab.next": ["meta+shift+bracketright", "ctrl+tab"],
  "tab.previous": ["meta+shift+bracketleft", "ctrl+shift+tab"],
  "tab.moveLeft": ["meta+shift+arrowleft"],
  "tab.moveRight": ["meta+shift+arrowright"],
  "pane.splitRight": ["meta+d"],
  "pane.splitDown": ["meta+shift+d"],
  "pane.close": ["meta+shift+w"],
  "pane.focusLeft": ["meta+alt+arrowleft"],
  "pane.focusRight": ["meta+alt+arrowright"],
  "pane.focusUp": ["meta+alt+arrowup"],
  "pane.focusDown": ["meta+alt+arrowdown"],
  "terminal.copy": ["meta+c"],
  "terminal.paste": ["meta+v"],
  "terminal.clear": ["meta+k"],
  "theme.cycle": [],
  "ssh.connect": ["meta+shift+o"],
  "hosts.toggle": ["meta+shift+h"],
  "hosts.import": [],
  "vault.open": [],
  "vault.lock": [],
  "history.open": ["meta+shift+r"],
  "forwards.open": [],
  "files.open": [],
  "snippets.open": [],
  "broadcast.toggle": [],
  "broadcast.tab": [],
  "broadcast.off": [],
  "workspaces.open": [],
  "serial.open": [],
};

export function defaultBindings(platform: Platform): Bindings {
  return platform === "mac" ? MAC : OTHER;
}

export function detectPlatform(userAgent: string): Platform {
  return /\bMac OS X\b|\bMacintosh\b/.test(userAgent) ? "mac" : "other";
}

const MODIFIERS = ["ctrl", "alt", "shift", "meta"] as const;
type Modifier = (typeof MODIFIERS)[number];

/** Named keys accepted in bindings, besides single letters and digits. */
const NAMED_KEYS = new Set([
  "tab",
  "enter",
  "escape",
  "space",
  "backspace",
  "delete",
  "home",
  "end",
  "pageup",
  "pagedown",
  "arrowleft",
  "arrowright",
  "arrowup",
  "arrowdown",
  "bracketleft",
  "bracketright",
  "comma",
  "period",
  "slash",
  "minus",
  "equal",
  ...Array.from({ length: 12 }, (_, i) => `f${i + 1}`),
]);

/**
 * Parses "Ctrl+Shift+T" into the normalized chord "ctrl+shift+t".
 * Returns `undefined` for anything malformed: unknown key, repeated or
 * missing key, or a modifier on its own.
 */
export function parseChord(text: string): string | undefined {
  const parts = text
    .toLowerCase()
    .split("+")
    .map((part) => part.trim());
  const key = parts.pop();
  if (!key || !isKey(key)) {
    return undefined;
  }
  const modifiers = new Set<Modifier>();
  for (const part of parts) {
    const modifier = toModifier(part);
    if (!modifier || modifiers.has(modifier)) {
      return undefined;
    }
    modifiers.add(modifier);
  }
  return format(modifiers, key);
}

/** The normalized chord for a key event, or `undefined` for a bare modifier press. */
export function chordOf(
  event: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">,
): string | undefined {
  const key = keyOf(event);
  if (!key) {
    return undefined;
  }
  const modifiers = new Set<Modifier>();
  if (event.ctrlKey) modifiers.add("ctrl");
  if (event.altKey) modifiers.add("alt");
  if (event.shiftKey) modifiers.add("shift");
  if (event.metaKey) modifiers.add("meta");
  return format(modifiers, key);
}

export interface Keymap {
  /** The action bound to the event's chord, if any. O(1). */
  match(event: Parameters<typeof chordOf>[0]): Action | undefined;
  /** Chords bound to an action, for display in menus and the palette. */
  chords(action: Action): readonly string[];
}

export interface KeymapProblem {
  action: string;
  binding: string;
  reason: string;
}

/**
 * Builds a keymap from defaults and user overrides. An override replaces an
 * action's chords entirely; `[]` unbinds it. Invalid entries are skipped and
 * reported, so one typo cannot break every shortcut. When two actions claim
 * a chord, the later one in `ACTIONS` order loses and is reported.
 *
 * O(a + b) to build for a actions and b bindings.
 */
export function buildKeymap(
  defaults: Bindings,
  overrides: unknown = {},
): { keymap: Keymap; problems: KeymapProblem[] } {
  const problems: KeymapProblem[] = [];
  const user = validOverrides(overrides, problems);
  const byChord = new Map<string, Action>();
  const byAction = new Map<Action, string[]>(ACTIONS.map((action) => [action, []]));
  const userClaimed = new Set<string>();

  const claim = (action: Action, binding: string, fromUser: boolean) => {
    const chord = parseChord(binding);
    if (!chord) {
      problems.push({ action, binding, reason: "not a valid key chord" });
      return;
    }
    const owner = byChord.get(chord);
    if (owner === action) {
      return; // the same chord written twice
    }
    if (owner) {
      // A default yields silently to a chord the user took on purpose.
      if (fromUser || !userClaimed.has(chord)) {
        problems.push({ action, binding, reason: `already bound to ${owner}` });
      }
      return;
    }
    byChord.set(chord, action);
    if (fromUser) userClaimed.add(chord);
    byAction.get(action)?.push(chord);
  };

  // User bindings first, so they win over defaults; then defaults for every
  // action the user did not rebind.
  for (const action of ACTIONS) {
    for (const binding of user[action] ?? []) claim(action, binding, true);
  }
  for (const action of ACTIONS) {
    if (user[action]) continue;
    for (const binding of defaults[action] ?? []) claim(action, binding, false);
  }

  return {
    keymap: {
      match: (event) => {
        const chord = chordOf(event);
        return chord ? byChord.get(chord) : undefined;
      },
      chords: (action) => byAction.get(action) ?? [],
    },
    problems,
  };
}

/** "ctrl+shift+t" -> "Ctrl+Shift+T", or "⌘⇧T" style on macOS. */
export function displayChord(chord: string, platform: Platform): string {
  const parts = chord.split("+");
  const key = parts.pop() ?? "";
  const label = KEY_LABELS[key] ?? key.toUpperCase();
  if (platform === "mac") {
    const symbols: Record<string, string> = { ctrl: "⌃", alt: "⌥", shift: "⇧", meta: "⌘" };
    return parts.map((m) => symbols[m] ?? m).join("") + label;
  }
  const names: Record<string, string> = { ctrl: "Ctrl", alt: "Alt", shift: "Shift", meta: "Super" };
  return [...parts.map((m) => names[m] ?? m), label].join("+");
}

const KEY_LABELS: Record<string, string> = {
  arrowleft: "←",
  arrowright: "→",
  arrowup: "↑",
  arrowdown: "↓",
  pageup: "PgUp",
  pagedown: "PgDn",
  bracketleft: "[",
  bracketright: "]",
  tab: "Tab",
  enter: "Enter",
  escape: "Esc",
  space: "Space",
};

function validOverrides(overrides: unknown, problems: KeymapProblem[]): Bindings {
  if (typeof overrides !== "object" || overrides === null || Array.isArray(overrides)) {
    if (overrides !== undefined) {
      problems.push({
        action: "*",
        binding: JSON.stringify(overrides) ?? "undefined",
        reason: "overrides must be an object",
      });
    }
    return {};
  }
  const valid: Bindings = {};
  for (const [action, value] of Object.entries(overrides)) {
    if (!isAction(action)) {
      problems.push({ action, binding: JSON.stringify(value), reason: "unknown action" });
      continue;
    }
    if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) {
      problems.push({
        action,
        binding: String(value),
        reason: "bindings must be a list of strings",
      });
      continue;
    }
    valid[action] = value;
  }
  return valid;
}

function isAction(value: string): value is Action {
  return (ACTIONS as readonly string[]).includes(value);
}

function isKey(key: string): boolean {
  return /^[a-z0-9]$/.test(key) || NAMED_KEYS.has(key);
}

function toModifier(part: string): Modifier | undefined {
  switch (part) {
    case "ctrl":
    case "control":
      return "ctrl";
    case "alt":
    case "option":
      return "alt";
    case "shift":
      return "shift";
    case "meta":
    case "cmd":
    case "command":
    case "super":
      return "meta";
    default:
      return undefined;
  }
}

function format(modifiers: ReadonlySet<Modifier>, key: string): string {
  return [...MODIFIERS.filter((m) => modifiers.has(m)), key].join("+");
}

/**
 * The key name for a chord. A printed ASCII letter or digit wins, so
 * shortcuts follow the label on the key on AZERTY or Dvorak, as in browsers.
 * When `key` is not plain ASCII (an input method such as Telex reports
 * "Process", Cyrillic reports its own letters, macOS Option composes
 * characters), the physical key from `code` is used instead.
 */
function keyOf(event: Pick<KeyboardEvent, "key" | "code">): string | undefined {
  const { key, code } = event;
  if (/^[A-Za-z0-9]$/.test(key)) return key.toLowerCase();
  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  const named = CODE_NAMES[code];
  if (named) return named;
  const lowered = key.toLowerCase();
  if (
    lowered === "control" ||
    lowered === "alt" ||
    lowered === "shift" ||
    lowered === "meta" ||
    lowered === "os"
  ) {
    return undefined;
  }
  return isKey(lowered) ? lowered : undefined;
}

const CODE_NAMES: Record<string, string> = {
  Tab: "tab",
  Enter: "enter",
  Escape: "escape",
  Space: "space",
  Backspace: "backspace",
  Delete: "delete",
  Home: "home",
  End: "end",
  PageUp: "pageup",
  PageDown: "pagedown",
  ArrowLeft: "arrowleft",
  ArrowRight: "arrowright",
  ArrowUp: "arrowup",
  ArrowDown: "arrowdown",
  BracketLeft: "bracketleft",
  BracketRight: "bracketright",
  Comma: "comma",
  Period: "period",
  Slash: "slash",
  Minus: "minus",
  Equal: "equal",
  ...Object.fromEntries(Array.from({ length: 12 }, (_, i) => [`F${i + 1}`, `f${i + 1}`])),
};
