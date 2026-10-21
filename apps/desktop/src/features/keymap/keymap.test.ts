// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import {
  ACTIONS,
  buildKeymap,
  chordOf,
  defaultBindings,
  detectPlatform,
  displayChord,
  parseChord,
} from "./keymap.ts";

const event = (
  code: string,
  key: string,
  mods: Partial<Record<"ctrl" | "alt" | "shift" | "meta", boolean>> = {},
) => ({
  code,
  key,
  ctrlKey: mods.ctrl ?? false,
  altKey: mods.alt ?? false,
  shiftKey: mods.shift ?? false,
  metaKey: mods.meta ?? false,
});

describe("parseChord", () => {
  it.each([
    ["Ctrl+Shift+T", "ctrl+shift+t"],
    ["shift+ctrl+t", "ctrl+shift+t"],
    ["Cmd+K", "meta+k"],
    ["Option+ArrowLeft", "alt+arrowleft"],
    ["F5", "f5"],
    [" ctrl + 1 ", "ctrl+1"],
  ])("%s -> %s", (text, chord) => {
    expect(parseChord(text)).toBe(chord);
  });

  it.each(["", "ctrl", "ctrl+", "ctrl+ctrl+t", "hyper+t", "ctrl+tt", "ctrl+é"])(
    "rejects %j",
    (text) => {
      expect(parseChord(text)).toBeUndefined();
    },
  );
});

describe("chordOf", () => {
  it("follows the printed letter on Latin layouts", () => {
    // AZERTY: the key printed W sits where US has Z.
    expect(chordOf(event("KeyZ", "W", { ctrl: true, shift: true }))).toBe("ctrl+shift+w");
    // Dvorak: the key printed J sits where US has C.
    expect(chordOf(event("KeyC", "J", { ctrl: true, shift: true }))).toBe("ctrl+shift+j");
  });

  it("falls back to the physical key when an input method hides the letter", () => {
    expect(chordOf(event("KeyD", "Process", { ctrl: true, shift: true }))).toBe("ctrl+shift+d");
    expect(chordOf(event("KeyD", "Đ", { ctrl: true, shift: true }))).toBe("ctrl+shift+d");
    expect(chordOf(event("KeyD", "в", { ctrl: true }))).toBe("ctrl+d");
    expect(chordOf(event("KeyK", "˚", { alt: true }))).toBe("alt+k");
  });

  it("names shifted digits by their key", () => {
    expect(chordOf(event("Digit1", "!", { ctrl: true, shift: true }))).toBe("ctrl+shift+1");
  });

  it("ignores a bare modifier press", () => {
    expect(chordOf(event("ShiftLeft", "Shift", { shift: true }))).toBeUndefined();
  });

  it("names arrows and page keys", () => {
    expect(chordOf(event("ArrowLeft", "ArrowLeft", { alt: true }))).toBe("alt+arrowleft");
    expect(chordOf(event("PageDown", "PageDown", { ctrl: true }))).toBe("ctrl+pagedown");
  });
});

describe("buildKeymap", () => {
  it("default bindings are valid and conflict-free on both platforms", () => {
    for (const platform of ["mac", "other"] as const) {
      const { problems } = buildKeymap(defaultBindings(platform));
      expect(problems).toEqual([]);
    }
  });

  it("defaults cover every action key so overrides can name any of them", () => {
    for (const platform of ["mac", "other"] as const) {
      expect(Object.keys(defaultBindings(platform)).toSorted()).toEqual(ACTIONS.toSorted());
    }
  });

  it("leaves plain Ctrl+K, Ctrl+C and Ctrl+D to the shell on Linux and Windows", () => {
    const { keymap } = buildKeymap(defaultBindings("other"));
    for (const code of ["KeyK", "KeyC", "KeyD", "KeyW"]) {
      expect(
        keymap.match(event(code, code.slice(3).toLowerCase(), { ctrl: true })),
      ).toBeUndefined();
    }
  });

  it("matches the palette shortcut", () => {
    const { keymap } = buildKeymap(defaultBindings("other"));
    expect(keymap.match(event("KeyP", "P", { ctrl: true, shift: true }))).toBe("palette.open");
  });

  it("applies overrides and unbinds with an empty list", () => {
    const { keymap, problems } = buildKeymap(defaultBindings("other"), {
      "tab.new": ["ctrl+alt+n"],
      "pane.close": [],
    });
    expect(problems).toEqual([]);
    expect(keymap.match(event("KeyN", "n", { ctrl: true, alt: true }))).toBe("tab.new");
    expect(keymap.match(event("KeyT", "T", { ctrl: true, shift: true }))).toBeUndefined();
    expect(keymap.chords("pane.close")).toEqual([]);
  });

  it("reports invalid overrides and keeps the rest working", () => {
    const { keymap, problems } = buildKeymap(defaultBindings("other"), {
      "tab.new": ["ctrl+banana"],
      "nope.action": ["ctrl+x"],
      "tab.close": "ctrl+q",
    });
    expect(problems.map((p) => p.reason)).toEqual([
      "unknown action",
      "bindings must be a list of strings",
      "not a valid key chord",
    ]);
    expect(keymap.match(event("KeyP", "P", { ctrl: true, shift: true }))).toBe("palette.open");
  });

  it("lets a user binding take a chord from a default", () => {
    const { keymap, problems } = buildKeymap(defaultBindings("other"), {
      "pane.close": ["ctrl+shift+w"],
    });
    expect(problems).toEqual([]);
    expect(keymap.match(event("KeyW", "W", { ctrl: true, shift: true }))).toBe("pane.close");
    expect(keymap.chords("tab.close")).toEqual([]);
  });

  it("reports two user bindings for one chord and keeps the earlier action", () => {
    const { keymap, problems } = buildKeymap(defaultBindings("other"), {
      "palette.open": ["ctrl+alt+p"],
      "tab.new": ["ctrl+alt+p"],
    });
    expect(problems).toEqual([
      { action: "tab.new", binding: "ctrl+alt+p", reason: "already bound to palette.open" },
    ]);
    expect(keymap.match(event("KeyP", "p", { ctrl: true, alt: true }))).toBe("palette.open");
  });

  it("drops the same chord written twice for one action", () => {
    const { keymap, problems } = buildKeymap(defaultBindings("other"), {
      "tab.new": ["ctrl+shift+t", "Ctrl+Shift+T"],
    });
    expect(problems).toEqual([]);
    expect(keymap.chords("tab.new")).toEqual(["ctrl+shift+t"]);
  });

  it("keeps Cmd+K for clearing the scrollback on macOS", () => {
    const { keymap } = buildKeymap(defaultBindings("mac"));
    expect(keymap.match(event("KeyK", "k", { meta: true }))).toBe("terminal.clear");
  });

  it.each([null, 42, "x", ["a"]])("treats non-object overrides %j as a problem", (overrides) => {
    const { problems } = buildKeymap(defaultBindings("other"), overrides);
    expect(problems).toHaveLength(1);
  });
});

describe("platform helpers", () => {
  it("detects macOS from the user agent", () => {
    expect(
      detectPlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15"),
    ).toBe("mac");
    expect(detectPlatform("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15")).toBe("other");
  });

  it("formats chords for each platform", () => {
    expect(displayChord("ctrl+shift+t", "other")).toBe("Ctrl+Shift+T");
    // Normalized order ctrl, alt, shift, meta matches Apple's ⌃⌥⇧⌘ convention.
    expect(displayChord(parseChord("cmd+shift+]".replace("]", "bracketright"))!, "mac")).toBe(
      "⇧⌘]",
    );
    expect(displayChord("alt+arrowleft", "other")).toBe("Alt+←");
  });
});
