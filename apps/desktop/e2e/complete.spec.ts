// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Completing the command being typed from the bundled specs, against the
// fake shell, whose `integrate` makes its prompts carry OSC 133 marks.

import { type Page, expect, test } from "@playwright/test";

const pane = (page: Page) => page.locator('section[data-pane-id][data-focused="true"]');
const screen = (page: Page) => pane(page).locator(".xterm-rows");
const list = (page: Page) => pane(page).getByTestId("completion");

async function run(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("completes a subcommand at a marked prompt, leaving Enter to the user", async ({ page }) => {
  await run(page, "integrate");
  await page.keyboard.type("git checkou");
  await expect(screen(page)).toContainText("$ git checkou");
  await page.keyboard.press("Control+Shift+Space");
  await expect(list(page).getByRole("button", { name: /^checkout/u })).toBeVisible();
  await list(page)
    .getByRole("button", { name: /^checkout/u })
    .click();
  await expect(list(page)).toHaveCount(0);
  await expect(screen(page)).toContainText("$ git checkout");
  await expect(screen(page)).not.toContainText("command not found");
});

test("opens the list under the word being completed", async ({ page }) => {
  await run(page, "integrate");
  await page.keyboard.type("git checkou");
  await page.keyboard.press("Control+Shift+Space");
  await expect(list(page)).toBeVisible();
  const cursor = await screen(page).locator(".xterm-cursor").boundingBox();
  const box = await list(page).boundingBox();
  if (!cursor || !box) throw new Error("no cursor or list on screen");
  // The cursor sits after "checkou", seven cells past where the word starts.
  expect(Math.abs(box.x - (cursor.x - 7 * cursor.width))).toBeLessThan(1.5);
  expect(Math.abs(box.y - (cursor.y + cursor.height))).toBeLessThan(1.5);
});

test("Tab takes the selection while the list shows, and other keys reach the shell", async ({
  page,
}) => {
  await run(page, "integrate");
  await page.keyboard.type("systemctl resta");
  await expect(screen(page)).toContainText("$ systemctl resta");
  await page.keyboard.press("Control+Shift+Space");
  await expect(list(page)).toBeVisible();
  await page.keyboard.press("Tab");
  await expect(list(page)).toHaveCount(0);
  await expect(screen(page)).toContainText("$ systemctl restart");
  await page.keyboard.press("Control+Shift+Space");
  await expect(list(page)).toHaveCount(0);
  await page.keyboard.type("x");
  await expect(screen(page)).toContainText("$ systemctl restart x");
});

test("offers nothing at a prompt the shell does not mark", async ({ page }) => {
  await page.keyboard.type("git checkou");
  await page.keyboard.press("Control+Shift+Space");
  await page.waitForTimeout(300);
  await expect(list(page)).toHaveCount(0);
});
