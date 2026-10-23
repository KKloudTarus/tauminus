// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Workspaces: the open tabs and their splits, saved under a name and
// opened again, also after a reload.

import { type Page, expect, test } from "@playwright/test";

async function palette(page: Page, command: string) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

test("saves the open tabs and opens them again after a reload", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".xterm-rows").first()).toContainText("$");
  await palette(page, "Split pane right");
  await palette(page, "New tab");
  await expect(page.getByRole("tab")).toHaveCount(2);

  await palette(page, "Workspaces: save or open a set of tabs");
  const dialog = page.getByRole("dialog", { name: "Workspaces" });
  await dialog.getByLabel("Workspace name").fill("dev");
  await dialog.getByRole("button", { name: "Save tabs" }).click();
  await expect(dialog.getByRole("listitem").filter({ hasText: "dev" })).toContainText(
    "2 tabs, 3 panes",
  );
  await dialog.getByRole("button", { name: "Close" }).click();

  await page.reload();
  await expect(page.getByRole("tab")).toHaveCount(1);
  await palette(page, "Open workspace: dev");
  await expect(page.getByRole("tab")).toHaveCount(3);
  await page.getByRole("tab").nth(1).click();
  await expect(page.locator("section[data-pane-id]")).toHaveCount(2);
});
