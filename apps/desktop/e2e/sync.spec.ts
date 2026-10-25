// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Sync against the in-memory one in `src/ipc/fake-sync.ts`, which has one
// other device behind every target.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const vault = (page: Page) => page.getByRole("dialog", { name: "Vault" });
const sync = (page: Page) => page.getByRole("dialog", { name: "Sync" });

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".xterm-rows").first()).toContainText("$");
});

test("asks for the vault, joins another device, and reviews its host", async ({ page }) => {
  await sidebar(page).getByRole("button", { name: "Sync", exact: true }).click();
  await sync(page).getByRole("button", { name: "Open the vault" }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct horse");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();

  await sidebar(page).getByRole("button", { name: "Sync", exact: true }).click();
  await sync(page).getByRole("button", { name: "Choose…" }).click();
  await sync(page)
    .getByRole("radio", { name: /Joins one/ })
    .check();
  await sync(page).getByLabel("Sync passphrase").fill("not their passphrase");
  await sync(page).getByRole("button", { name: "Join" }).click();
  await expect(sync(page).getByRole("alert")).toContainText("does not open");
  await sync(page).getByLabel("Sync passphrase").fill("correct horse battery");
  await sync(page).getByRole("button", { name: "Join" }).click();
  await expect(sync(page)).toContainText("/home/me/Sync/tauminus");

  await sync(page).getByRole("button", { name: "Sync now" }).click();
  await expect(sync(page)).toContainText("Last synced");
  const staging = sync(page).getByRole("article", { name: "staging" });
  await expect(staging).toContainText('ProxyCommand("nc %h %p")');
  await staging.getByRole("button", { name: "Accept staging" }).click();
  await expect(sync(page).getByRole("region", { name: "Changes to review" })).toHaveCount(0);

  await sync(page)
    .getByRole("button", { name: /Stop syncing on this device/ })
    .click();
  await sync(page).getByRole("button", { name: "Stop syncing here" }).click();
  await expect(sync(page).getByRole("button", { name: "Start syncing" })).toBeVisible();
});

test("opens from the command palette", async ({ page }) => {
  await page.keyboard.press("Control+Shift+P");
  await page.getByRole("combobox").fill("sync hosts");
  await page.keyboard.press("Enter");
  await expect(sync(page)).toBeVisible();
});
