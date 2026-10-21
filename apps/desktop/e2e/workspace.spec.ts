// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type Locator, type Page, expect, test } from "@playwright/test";

const panes = (page: Page) => page.locator("section[data-pane-id]");
const focusedPane = (page: Page) => page.locator('section[data-pane-id][data-focused="true"]');
const tabs = (page: Page) => page.getByRole("tab");
const screenOf = (pane: Locator) => pane.locator(".xterm-rows");

async function typeIn(page: Page, text: string) {
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screenOf(focusedPane(page))).toContainText("$");
  await focusedPane(page).click();
});

test("runs commands in the first terminal", async ({ page }) => {
  await typeIn(page, "echo hello from e2e");
  await expect(screenOf(focusedPane(page))).toContainText("hello from e2e");
});

test("opens, cycles and closes tabs with shortcuts", async ({ page }) => {
  await page.keyboard.press("Control+Shift+T");
  await expect(tabs(page)).toHaveCount(2);
  await expect(tabs(page).nth(1)).toHaveAttribute("aria-selected", "true");

  await page.keyboard.press("Control+Tab");
  await expect(tabs(page).nth(0)).toHaveAttribute("aria-selected", "true");

  await page.keyboard.press("Control+Shift+W");
  await expect(tabs(page)).toHaveCount(1);
});

test("keeps each tab's screen when switching tabs", async ({ page }) => {
  await typeIn(page, "echo first-tab");
  await page.keyboard.press("Control+Shift+T");
  await expect(screenOf(focusedPane(page))).not.toContainText("first-tab");
  await page.keyboard.press("Control+Shift+Tab");
  await expect(screenOf(focusedPane(page))).toContainText("first-tab");
});

test("splits panes and moves focus between them", async ({ page }) => {
  await page.keyboard.press("Control+Shift+D");
  await expect(panes(page)).toHaveCount(2);
  const right = await focusedPane(page).getAttribute("data-pane-id");
  await typeIn(page, "echo right-pane");

  await page.keyboard.press("Alt+ArrowLeft");
  await expect(focusedPane(page)).not.toHaveAttribute("data-pane-id", right ?? "");
  await typeIn(page, "echo left-pane");

  await expect(page.locator(`section[data-pane-id="${right}"] .xterm-rows`)).toContainText(
    "right-pane",
  );
  await expect(page.locator(`section[data-pane-id="${right}"] .xterm-rows`)).not.toContainText(
    "left-pane",
  );

  await page.keyboard.press("Control+Shift+E");
  await expect(panes(page)).toHaveCount(3);
});

test("closes a pane whose shell exits cleanly, and keeps one that fails", async ({ page }) => {
  await page.keyboard.press("Control+Shift+D");
  await expect(panes(page)).toHaveCount(2);
  await typeIn(page, "exit 0");
  await expect(panes(page)).toHaveCount(1);

  await typeIn(page, "exit 3");
  await expect(panes(page)).toHaveCount(1);
  await expect(page.getByRole("status")).toContainText("Process exited with code 3");
});

test("shows the title a program sets", async ({ page }) => {
  await typeIn(page, "title deploy@prod-db-2");
  await expect(tabs(page).first()).toContainText("deploy@prod-db-2");
});

test("runs commands from the palette", async ({ page }) => {
  await page.keyboard.press("Control+Shift+P");
  const palette = page.getByRole("dialog");
  await expect(palette).toBeVisible();
  await page.keyboard.type("split pane right");
  await page.keyboard.press("Enter");
  await expect(palette).toBeHidden();
  await expect(panes(page)).toHaveCount(2);
});

test("switches theme from the palette and remembers it", async ({ page }) => {
  const html = page.locator("html");
  await page.keyboard.press("Control+Shift+P");
  await page.keyboard.type("switch theme");
  await page.keyboard.press("Enter");
  await expect(html).toHaveClass(/light/);
  await page.reload();
  await expect(html).toHaveClass(/light/);
});

test("reorders tabs by dragging", async ({ page }) => {
  await typeIn(page, "title alpha");
  await page.keyboard.press("Control+Shift+T");
  await focusedPane(page).click();
  await typeIn(page, "title beta");
  await expect(tabs(page)).toHaveText([/alpha/, /beta/]);

  await tabs(page).nth(1).dragTo(tabs(page).nth(0));
  await expect(tabs(page)).toHaveText([/beta/, /alpha/]);
});

test("leaves plain Ctrl+K to the shell", async ({ page }) => {
  await page.keyboard.press("Control+K");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});
