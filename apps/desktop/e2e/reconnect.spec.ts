// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// A lost connection: the pane says so, connects again on its own, and
// keeps what it showed.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

test("a dropped connection comes back by itself", async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("router");
  await editor.getByLabel("Address").fill("10.0.0.1");
  await editor.getByLabel("Protocol").selectOption("telnet");
  await editor.getByRole("button", { name: "Save" }).click();
  await sidebar(page)
    .getByRole("button", { name: /^router/ })
    .click();
  await expect(screen(page)).toContainText("Connected to router over Telnet");
  await page.keyboard.type("echo before");
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("before$");

  await page.keyboard.type("drop");
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("[connection lost]");
  await expect(page.getByTestId("reconnecting")).toContainText("Connection to router lost");
  await expect(screen(page)).toContainText("[connected again]", { timeout: 10_000 });
  await expect(page.getByTestId("reconnecting")).toHaveCount(0);
  // What the pane showed is still there, and the new session takes input.
  await expect(screen(page)).toContainText("before$");
  await page.keyboard.type("echo after");
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("after$");
});
