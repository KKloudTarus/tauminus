// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Zmodem against the fake in `src/ipc/fake.ts`: `sz NAME` offers NAME,
// which comes in a moment once accepted; `rz` waits for files, and the
// fake dialog picks `notes.txt`.

import { type Page, expect, test } from "@playwright/test";

const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');
const banner = (page: Page) => page.getByTestId("receive");

async function run(page: Page, line: string) {
  const input = page.getByRole("textbox", { name: "Terminal input" });
  await expect(input).toBeFocused();
  await page.keyboard.type(line);
  await page.keyboard.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("asks before receiving, then names the file and where it went", async ({ page }) => {
  await run(page, "sz report.pdf");
  await expect(banner(page)).toContainText("wants to send files");
  // Keys typed meanwhile do not reach the host.
  await page.keyboard.type("zzz");
  await banner(page).getByRole("button", { name: "Receive" }).click();
  await expect(banner(page)).toContainText("Received report.pdf");
  await expect(screen(page)).not.toContainText("zzz");
  await banner(page).getByRole("button", { name: "Close" }).click();
  await expect(banner(page)).toHaveCount(0);
  await run(page, "echo back");
  await expect(screen(page)).toContainText("back");
});

test("declining sends nothing and gives the prompt back", async ({ page }) => {
  await run(page, "sz notes.txt");
  await banner(page).getByRole("button", { name: "Decline" }).click();
  await expect(banner(page)).toHaveCount(0);
  await run(page, "echo after");
  await expect(screen(page)).toContainText("after");
});

test("sends what the user chose to rz, and says what went", async ({ page }) => {
  await run(page, "rz");
  await expect(banner(page)).toContainText("waits for files (rz)");
  await banner(page).getByRole("button", { name: "Choose files…" }).click();
  await expect(banner(page)).toContainText("Sent notes.txt");
  await expect(banner(page).getByRole("button", { name: "Show folder" })).toHaveCount(0);
  await banner(page).getByRole("button", { name: "Close" }).click();
  await run(page, "echo done");
  await expect(screen(page)).toContainText("done");
});
