// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// A saved Mosh host: chosen in the editor, explained, and opened against
// the in-memory backend; files still go over SSH.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

test("a Mosh host opens over Mosh and keeps its files", async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("laptop-db");
  await editor.getByLabel("Address").fill("db.internal");
  await editor.getByLabel("Protocol").selectOption("mosh");
  await expect(editor).toContainText("UDP ports 60000 to 61000");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);

  await expect(sidebar(page).getByRole("button", { name: "Files on laptop-db" })).toHaveCount(1);
  await sidebar(page)
    .getByRole("button", { name: /^laptop-db/ })
    .click();
  await expect(screen(page)).toContainText("Connected to laptop-db over Mosh");
  await page.keyboard.type("echo over mosh");
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("over mosh$");

  await sidebar(page).getByRole("button", { name: "Edit laptop-db" }).click();
  await expect(
    page.getByRole("dialog", { name: "Edit laptop-db" }).getByLabel("Protocol"),
  ).toHaveValue("mosh");
});
