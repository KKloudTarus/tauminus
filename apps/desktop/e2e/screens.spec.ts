// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Screenshots of the main screens, light and dark, desktop and phone widths.
// Opt-in: TAUMINUS_SCREENS=<dir> pnpm e2e screens.spec.ts writes PNGs there.
// They are for review, not pixel assertions.

import { type Page, expect, test } from "@playwright/test";

const OUT = process.env["TAUMINUS_SCREENS"];
test.skip(!OUT, "set TAUMINUS_SCREENS=<dir> to capture screenshots");

const SIZES = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "phone", width: 390, height: 844 },
] as const;
const THEMES = ["dark", "light"] as const;

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const focusedScreen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

async function seedHosts(page: Page) {
  await sidebar(page)
    .getByRole("button", { name: /^Import 3 hosts/ })
    .click();
  await expect(sidebar(page).getByRole("status")).toHaveText(/Imported 3 hosts/);
  await sidebar(page).getByRole("button", { name: "Dismiss" }).click();
  for (const [name, address, tags] of [
    ["api-prod-1", "10.0.4.11", "prod, api"],
    ["db-primary", "db1.internal", "prod, postgres"],
    ["staging-web", "staging.example.com", "staging"],
  ] as const) {
    // oxlint-disable-next-line no-await-in-loop -- one editor open at a time
    await sidebar(page).getByRole("button", { name: "New host" }).click();
    const dialog = page.getByRole("dialog", { name: "New host" });
    // oxlint-disable-next-line no-await-in-loop -- one editor open at a time
    await dialog.getByLabel("Name").fill(name);
    // oxlint-disable-next-line no-await-in-loop -- one editor open at a time
    await dialog.getByLabel("Address").fill(address);
    // oxlint-disable-next-line no-await-in-loop -- one editor open at a time
    await dialog.getByLabel("Tags (comma-separated)").fill(tags);
    // oxlint-disable-next-line no-await-in-loop -- one editor open at a time
    await dialog.getByRole("button", { name: "Save" }).click();
    // oxlint-disable-next-line no-await-in-loop -- one editor open at a time
    await expect(dialog).toHaveCount(0);
  }
}

async function shot(page: Page, name: string) {
  // Let transitions settle so the image shows the resting state.
  await page.waitForTimeout(250);
  await page.screenshot({ path: `${OUT}/${name}.png` });
}

for (const size of SIZES) {
  for (const theme of THEMES) {
    test(`${size.name} ${theme}`, async ({ page }) => {
      test.setTimeout(90_000);
      await page.setViewportSize({ width: size.width, height: size.height });
      await page.addInitScript((t) => localStorage.setItem("tauminus.theme", t), theme);
      await page.goto("/");
      await expect(focusedScreen(page)).toContainText("$");
      const prefix = `${size.name}-${theme}`;
      if (await sidebar(page).isVisible()) await shot(page, `${prefix}-01-first-run`);
      else await shot(page, `${prefix}-01-terminal`);

      if (!(await sidebar(page).isVisible())) {
        await page.getByRole("button", { name: "Hosts", exact: true }).click();
      }
      await seedHosts(page);
      await shot(page, `${prefix}-02-hosts`);

      await sidebar(page).getByRole("button", { name: "New host" }).click();
      await shot(page, `${prefix}-03-host-editor`);
      await page.keyboard.press("Escape");

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").waitFor();
      await shot(page, `${prefix}-04-palette`);
      await page.keyboard.press("Escape");

      await sidebar(page)
        .getByRole("button", { name: /^prompt alice@prompt\.test/ })
        .click();
      await shot(page, `${prefix}-05-host-key`);
      await page.getByRole("button", { name: "Trust once" }).click();
      const password = page.getByRole("dialog", { name: "Password for alice@prompt.test" });
      await password.getByLabel("Password", { exact: true }).fill("secret");
      await password.getByLabel("Password", { exact: true }).press("Enter");
      await expect(focusedScreen(page)).toContainText("Welcome to prompt.test");
      await shot(page, `${prefix}-06-connected`);

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").fill("split pane right");
      await page.keyboard.press("Enter");
      await shot(page, `${prefix}-07-split`);

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").fill("open the vault");
      await page.keyboard.press("Enter");
      await shot(page, `${prefix}-08-vault`);
      await page.keyboard.press("Escape");

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").fill("snippets");
      await page.keyboard.press("Enter");
      await shot(page, `${prefix}-09-snippets`);
      await page.keyboard.press("Escape");

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").fill("recordings: play back");
      await page.keyboard.press("Enter");
      await shot(page, `${prefix}-11-recordings`);
      await page.keyboard.press("Escape");

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").fill("Highlight sets and triggers");
      await page.keyboard.press("Enter");
      await page.getByRole("button", { name: "New set" }).click();
      await shot(page, `${prefix}-12-highlights`);
      await page.keyboard.press("Escape");
      await page.keyboard.press("Escape");

      await page.locator("body").press("Control+Shift+P");
      await page.getByPlaceholder("Type a command").fill("connect to ssh host");
      await page.keyboard.press("Enter");
      await shot(page, `${prefix}-10-connect`);
      await page.keyboard.press("Escape");
    });
  }
}
