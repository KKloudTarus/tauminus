// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

export type ThemePreference = "system" | "light" | "dark";

const STORAGE_KEY = "tauminus.theme";
const ORDER: readonly ThemePreference[] = ["system", "light", "dark"];

export function nextTheme(current: ThemePreference): ThemePreference {
  return ORDER[(ORDER.indexOf(current) + 1) % ORDER.length] ?? "system";
}

export function loadTheme(storage: Pick<Storage, "getItem"> | undefined): ThemePreference {
  try {
    const value = storage?.getItem(STORAGE_KEY);
    return value === "light" || value === "dark" || value === "system" ? value : "system";
  } catch {
    // Storage can be unavailable; the system theme is a safe default.
    return "system";
  }
}

export function saveTheme(
  storage: Pick<Storage, "setItem"> | undefined,
  theme: ThemePreference,
): void {
  try {
    storage?.setItem(STORAGE_KEY, theme);
  } catch {
    // A preference that does not persist is not worth surfacing.
  }
}

/**
 * Applies the preference as a class on the root element. `system` removes
 * both classes and lets the `prefers-color-scheme` media query decide.
 */
export function applyTheme(root: HTMLElement, theme: ThemePreference): void {
  root.classList.toggle("light", theme === "light");
  root.classList.toggle("dark", theme === "dark");
}
