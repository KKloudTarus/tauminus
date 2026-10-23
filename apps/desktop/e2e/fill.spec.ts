// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Typing a saved host's password at its prompt, against the fake vault and
// the fake shell's `vault.test` host, whose identity's password is
// `FAKE_IDENTITY_PASSWORD` in `src/ipc/fake.ts`.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const vault = (page: Page) => page.getByRole("dialog", { name: "Vault" });
const pane = (page: Page) => page.locator('section[data-pane-id][data-focused="true"]');
const screen = (page: Page) => pane(page).locator(".xterm-rows");

async function run(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

/** A vault, and a saved host on `vault.test`, connected. */
async function connected(page: Page) {
  await sidebar(page).getByRole("button", { name: "Vault", exact: true }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct horse");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("vaulted");
  await editor.getByLabel("Address").fill("vault.test");
  await editor.getByRole("button", { name: "Save" }).click();
  await sidebar(page)
    .getByRole("button", { name: /^vaulted/ })
    .click();
  await expect(screen(page)).toContainText("signed in from the vault");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("types the host's password at sudo's prompt, and leaves Enter to the user", async ({
  page,
}) => {
  await connected(page);
  await run(page, "sudo");
  const fill = pane(page).getByTestId("password-fill");
  await expect(fill).toHaveText("Type the password of deploy (me@vault.test)");
  await fill.click();
  await expect(fill).toHaveCount(0);
  // Not echoed, and not sent on until Enter.
  await expect(screen(page)).not.toContainText("hunter2");
  await expect(screen(page)).not.toContainText("sudo: ok");
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("sudo: ok");
});

test("the shortcut types it too", async ({ page }) => {
  await connected(page);
  await run(page, "sudo");
  await expect(pane(page).getByTestId("password-fill")).toBeVisible();
  await page.keyboard.press("Control+Shift+L");
  await expect(pane(page).getByTestId("password-fill")).toHaveCount(0);
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("sudo: ok");
});

test("warns when the host shows what was typed", async ({ page }) => {
  await connected(page);
  // A question that only looks like a password prompt: what is typed echoes.
  await run(page, "ask [sudo] password for me:");
  await pane(page).getByTestId("password-fill").click();
  await expect(pane(page).getByTestId("password-echoed")).toContainText(
    "may not have asked for a password",
  );
});

test("offers nothing in a local terminal", async ({ page }) => {
  await run(page, "sudo");
  await expect(screen(page)).toContainText("[sudo] password for me:");
  await page.waitForTimeout(300);
  await expect(pane(page).getByTestId("password-fill")).toHaveCount(0);
});
