// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Local shells: a tab in another shell from the palette, and a default shell
// that new tabs open, also after a reload. The fake backend lists PowerShell 7
// and Ubuntu under WSL, and a tab in one prints its name.

import { type Page, expect, test } from "@playwright/test";

async function palette(page: Page, command: string) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  // The shells' items come from the backend: until this one is the selected
  // item, Enter would run another, such as plain "New tab".
  const escaped = command.replaceAll(/[.*+?^${}()|[\]\\]/gu, String.raw`\$&`);
  await expect(page.getByRole("option", { selected: true })).toHaveAccessibleName(
    new RegExp(`^${escaped}( |$)`, "u"),
  );
  await input.press("Enter");
}

test("opens a tab in another shell, and new tabs in the default one", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".xterm-rows").first()).toContainText("$");

  await palette(page, "New tab: Ubuntu (WSL)");
  await expect(page.getByRole("tab")).toHaveCount(2);
  await expect(page.getByRole("tab", { selected: true })).toContainText("Ubuntu (WSL)");

  await palette(page, "Default shell: PowerShell 7");
  await palette(page, "New tab");
  await expect(page.getByRole("tab")).toHaveCount(3);
  await expect(page.getByRole("tab", { selected: true })).toContainText("PowerShell 7");

  // The choice outlives the window; the system default forgets it.
  await page.reload();
  await expect(page.getByRole("tab", { selected: true })).toContainText("PowerShell 7");
  await palette(page, "Default shell: system default");
  await palette(page, "New tab");
  await expect(page.getByRole("tab", { selected: true })).toContainText("Terminal");
});
