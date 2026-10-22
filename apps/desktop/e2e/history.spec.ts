// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The history against the in-memory one in `src/ipc/fake-history.ts`, which
// follows the shared fake vault.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const vault = (page: Page) => page.getByRole("dialog", { name: "Vault" });
const history = (page: Page) => page.getByRole("dialog", { name: "Command history" });
const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

async function run(page: Page, command: string) {
  // Focus comes back to the terminal a frame after a dialog closes.
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.type(command);
  // Enter after the echo, as a person types: the recorder takes keys the
  // screen does not show for a password typed with echo off.
  await expect(screen(page)).toContainText(command);
  await page.keyboard.press("Enter");
}

/**
 * Opens the history once it lists `command`: a command is recorded when
 * the next one starts, over IPC, so a dialog opened at once may search
 * before it lands.
 */
async function openHistoryWith(page: Page, command: string) {
  const dialog = history(page);
  await expect(async () => {
    if ((await dialog.count()) > 0) await dialog.getByRole("button", { name: "Close" }).click();
    await page.keyboard.press("Control+Shift+R");
    await expect(dialog.getByRole("list", { name: "Commands" })).toContainText(command, {
      timeout: 1000,
    });
  }).toPass();
  return dialog;
}

async function createVault(page: Page) {
  await sidebar(page).getByRole("button", { name: "Vault", exact: true }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct horse");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("finds commands by their output, filters by exit, and inserts one", async ({ page }) => {
  await createVault(page);
  await run(page, "echo needle in a haystack");
  await run(page, "fail disk full");
  await run(page, "echo done");
  const dialog = await openHistoryWith(page, "fail disk full");
  await dialog.getByLabel("Search commands and output").fill("haystack");
  const results = dialog.getByRole("list", { name: "Commands" });
  await expect(results.getByRole("listitem")).toHaveCount(1);
  await expect(results).toContainText("echo needle in a haystack");
  await results.getByRole("button", { name: "Show output" }).click();
  await expect(results.locator("pre")).toHaveText("needle in a haystack");

  await dialog.getByLabel("Search commands and output").fill("");
  await dialog.getByLabel("Exit status").selectOption("failure");
  await expect(results.getByRole("listitem")).toHaveCount(1);
  await expect(results).toContainText("fail disk full");
  await expect(results).toContainText("exit 2");

  await results.getByRole("button", { name: "Insert" }).click();
  await expect(dialog).toHaveCount(0);
  // Inserted, not run: it sits on the prompt line.
  await expect(screen(page)).toContainText("$ fail disk full");
});

test("a locked vault records but asks to unlock to search", async ({ page }) => {
  await sidebar(page).getByRole("button", { name: "Vault", exact: true }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct horse");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await vault(page).getByRole("button", { name: "Lock now" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();
  await run(page, "echo while locked");
  await run(page, "echo after");
  await page.keyboard.press("Control+Shift+R");
  const dialog = history(page);
  await dialog.getByRole("button", { name: "Unlock the vault" }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(dialog.getByRole("list", { name: "Commands" })).toContainText("echo while locked");
});

test("a password typed with echo off is not recorded", async ({ page }) => {
  await createVault(page);
  await run(page, "echo visible");
  // The fake shell echoes everything, so act out echo off: the keys reach
  // the recorder but the screen shows a password prompt line only.
  await page.keyboard.type("echo Password: ");
  await expect(screen(page)).toContainText("echo Password:");
  await page.keyboard.press("Enter");
  await run(page, "echo last");
  const results = (await openHistoryWith(page, "echo visible")).getByRole("list", {
    name: "Commands",
  });
  await expect(results).not.toContainText("Password");
});
