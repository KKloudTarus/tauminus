// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// A saved Telnet host: chosen in the editor, warned about, and opened
// without SSH's prompts, against the in-memory backend.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const focusedScreen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

test("a Telnet host opens without SSH prompts and has no files", async ({ page }) => {
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("router");
  await editor.getByLabel("Address").fill("10.0.0.1");
  await editor.getByLabel("Protocol").selectOption("telnet");
  await expect(editor).toContainText("clear text");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);

  await expect(sidebar(page).getByRole("button", { name: "Files on router" })).toHaveCount(0);
  await sidebar(page)
    .getByRole("button", { name: /^router/ })
    .click();
  await expect(focusedScreen(page)).toContainText("Connected to router over Telnet");
  await expect(page.getByRole("tab", { name: /^router/ })).toHaveAttribute("aria-selected", "true");
  await page.keyboard.type("echo over telnet");
  await page.keyboard.press("Enter");
  await expect(focusedScreen(page)).toContainText("echo over telnetover telnet$");
});
