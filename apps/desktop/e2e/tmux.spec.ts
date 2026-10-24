// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// tmux windows as tabs, against the fake in `src/ipc/fake.ts`: a host at
// `tmux.test` shows two windows, `shell` and `logs`; any other host's tmux
// is too old, and it joins as one terminal.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const tabs = (page: Page) => page.getByRole("tablist", { name: "Terminal tabs" }).getByRole("tab");
const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

async function tmuxHost(page: Page, name: string, address: string) {
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill(name);
  await editor.getByLabel("Address").fill(address);
  await editor.getByText("Session").click();
  await editor.getByLabel(/tmux to join/).fill("work");
  await editor.getByLabel(/windows as tabs/).selectOption("on");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);
}

async function palette(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("opens each tmux window as a tab, the first in front", async ({ page }) => {
  await tmuxHost(page, "work box", "tmux.test");
  await sidebar(page)
    .getByRole("button", { name: /^work box/u })
    .click();
  await expect(tabs(page)).toHaveCount(3);
  await expect(tabs(page).nth(1)).toContainText("work box");
  await expect(tabs(page).nth(2)).toContainText("work box: logs");
  await expect(screen(page)).toContainText("tmux pane %1");
  await tabs(page).nth(2).click();
  await expect(screen(page)).toContainText("tmux pane %2");
  // A new window comes back from tmux as a tab of its own.
  await page.getByRole("textbox", { name: "Terminal input" }).focus();
  await palette(page, "New tmux window");
  await expect(tabs(page)).toHaveCount(4);
  await expect(tabs(page).nth(3)).toContainText("work box: new");
});

test("a host whose tmux cannot show tabs joins as one terminal, and says why", async ({ page }) => {
  await tmuxHost(page, "old box", "old.test");
  await sidebar(page)
    .getByRole("button", { name: /^old box/u })
    .click();
  await expect(tabs(page)).toHaveCount(2);
  await expect(screen(page)).toContainText("Connected to old box, in tmux");
  await expect(page.getByText(/too old to show its windows as tabs/u)).toBeVisible();
});
