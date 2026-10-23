// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Highlight sets against the in-memory library in `src/ipc/fake-triggers.ts`.

import { type Page, expect, test } from "@playwright/test";

const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');
const dialog = (page: Page) => page.getByRole("dialog", { name: "Highlight sets" });

async function palette(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

async function run(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
/** Cells a decoration colours in the focused pane, by their text. */
const coloured = (page: Page) => screen(page).locator("span.xterm-decoration-top");

async function newSet(page: Page, name: string, pattern: string) {
  await palette(page, "Highlight sets and triggers");
  await dialog(page).getByRole("button", { name: "New set" }).click();
  await dialog(page).getByLabel("Name", { exact: true }).fill(name);
  await dialog(page).getByLabel("Rule 1 pattern").fill(pattern);
  await dialog(page).getByLabel("Rule 1 background").selectOption({ label: "Yellow" });
  await dialog(page).getByRole("button", { name: "Save" }).click();
  await expect(dialog(page).getByRole("list", { name: "Highlight sets" })).toContainText(name);
}

test("a set applied to every session colours what it matches, in every pane", async ({ page }) => {
  await newSet(page, "Errors", "ERROR|FAIL\\w*");
  await dialog(page).getByLabel("Apply Errors to every session").check();
  await dialog(page).getByRole("button", { name: "Close" }).click();
  await run(page, "echo an ERROR, then FAILED");
  await expect(coloured(page)).toHaveCount(2 * "ERRORFAILED".length);
  await expect(coloured(page).first()).toHaveCSS("background-color", "rgb(196, 160, 0)");

  // Not applied any more: the colour goes, without new output.
  await palette(page, "Highlight sets and triggers");
  await dialog(page).getByLabel("Apply Errors to every session").uncheck();
  await dialog(page).getByRole("button", { name: "Close" }).click();
  await expect(coloured(page)).toHaveCount(0);
});

test("a pattern that cannot be one says why before it is saved", async ({ page }) => {
  await palette(page, "Highlight sets and triggers");
  await dialog(page).getByRole("button", { name: "New set" }).click();
  await dialog(page).getByLabel("Name", { exact: true }).fill("Broken");
  await dialog(page).getByLabel("Rule 1 pattern").fill("(a)\\1");
  await expect(dialog(page).getByRole("alert")).toContainText("not valid");
  await dialog(page).getByRole("button", { name: "Save" }).click();
  await expect(dialog(page).getByRole("alert").first()).toBeVisible();
  await expect(dialog(page).getByRole("list", { name: "Highlight sets" })).not.toContainText(
    "Broken",
  );
});

test("a group's set colours its hosts' panes and no other", async ({ page }) => {
  await newSet(page, "Errors", "ERROR");
  await dialog(page).getByRole("button", { name: "Close" }).click();
  await sidebar(page).getByRole("button", { name: "Import 3 hosts from ~/.ssh/config" }).click();
  await sidebar(page).getByRole("button", { name: "Edit group SSH config" }).click();
  const group = page.getByRole("dialog", { name: "Edit group SSH config" });
  await group.getByText("Highlight sets", { exact: true }).click();
  await group.getByLabel("Add a highlight set").selectOption({ label: "Errors" });
  await expect(group.getByRole("list", { name: "Highlight sets" })).toContainText("Errors");
  await group.getByRole("button", { name: "Save" }).click();
  await expect(group).toHaveCount(0);

  // The local pane names no set.
  await page.locator('section[data-pane-id][data-focused="true"]').click();
  await run(page, "echo local ERROR");
  await expect(screen(page)).toContainText("local ERROR");

  await sidebar(page)
    .getByRole("button", { name: /^prompt alice@prompt\.test/ })
    .click();
  await page.getByRole("button", { name: "Trust once" }).click();
  const password = page.getByRole("dialog", { name: "Password for alice@prompt.test" });
  await password.getByLabel("Password", { exact: true }).fill("secret");
  await password.getByLabel("Password", { exact: true }).press("Enter");
  await expect(screen(page)).toContainText("Welcome to prompt.test");
  await run(page, "echo remote ERROR");
  await expect(coloured(page)).toHaveCount(2 * "ERROR".length);

  await page.getByRole("tab").first().click();
  await expect(screen(page)).toContainText("local ERROR");
  // Give a scan time to run, then check it found nothing to colour.
  await page.waitForTimeout(500);
  await expect(coloured(page)).toHaveCount(0);
});
