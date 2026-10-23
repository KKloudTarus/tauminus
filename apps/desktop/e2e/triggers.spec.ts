// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Triggers against the in-memory library in `src/ipc/fake-triggers.ts`,
// on the fake shell in `src/ipc/fake.ts`.

import { type Page, expect, test } from "@playwright/test";

const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');
const sets = (page: Page) => page.getByRole("dialog", { name: "Highlight sets" });
const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });

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

/** A set for every session with one rule doing `action`. */
async function rule(
  page: Page,
  pattern: string,
  action: string,
  fill?: (dialog: ReturnType<typeof sets>) => Promise<void>,
) {
  await palette(page, "Highlight sets and triggers");
  await sets(page).getByRole("button", { name: "New set" }).click();
  await sets(page).getByLabel("Name", { exact: true }).fill("Watch");
  await sets(page).getByLabel("Rule 1 pattern").fill(pattern);
  await sets(page).getByLabel("Rule 1 action").selectOption({ label: action });
  await fill?.(sets(page));
  await sets(page).getByRole("button", { name: "Save" }).click();
  await sets(page).getByLabel("Apply Watch to every session").check();
  await sets(page).getByRole("button", { name: "Close" }).click();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("a notify rule tells the user what matched, and where", async ({ page }) => {
  await rule(page, "^deploy done", "Notify me", async (dialog) => {
    await dialog.getByLabel("Rule 1 name").fill("Deploys");
  });
  await run(page, "echo deploy done in 3s");
  await expect(sidebar(page).getByRole("status")).toContainText(
    "Deploys on Terminal: “deploy done in 3s”",
  );
});

test("a capture rule keeps the line, with the secret hidden", async ({ page }) => {
  await rule(page, "^login ok", "Keep the line");
  await run(page, "echo login ok token=abc123");
  await expect(screen(page)).toContainText("login ok token=abc123");
  await palette(page, "Captured lines: what trigger rules kept");
  const captured = page
    .getByRole("dialog", { name: "Captured lines" })
    .getByRole("list", { name: "Captured lines" });
  await expect(captured).toContainText("login ok token=");
  await expect(captured).not.toContainText("abc123");
});

test("a send rule answers a question waiting on the screen, once", async ({ page }) => {
  await rule(page, String.raw`Continue\? \[y/N\]`, "Send text", async (dialog) => {
    await dialog.getByLabel("Rule 1 text to send").fill("yes");
  });
  await run(page, "ask Continue? [y/N]");
  await expect(screen(page)).toContainText("answer: yes");
  // The question, now a finished line, does not answer again.
  await page.waitForTimeout(800);
  await expect(screen(page).getByText("answer: yes")).toHaveCount(1);
});

test("a send rule waits while the user types", async ({ page }) => {
  await rule(page, String.raw`Continue\? \[y/N\]`, "Send text", async (dialog) => {
    await dialog.getByLabel("Rule 1 text to send").fill("yes");
  });
  await run(page, "ask Continue? [y/N]");
  // Typed before the rule's quiet time ran out: the rule keeps off the line.
  await page.keyboard.type("n");
  await page.waitForTimeout(800);
  await page.keyboard.press("Enter");
  await expect(screen(page)).toContainText("answer: n");
  await expect(screen(page)).not.toContainText("answer: yes");
  await expect(screen(page)).not.toContainText("answer: nyes");
});

test("a send rule types nothing at a password prompt", async ({ page }) => {
  await rule(page, String.raw`^\$ sudo$`, "Send text", async (dialog) => {
    await dialog.getByLabel("Rule 1 text to send").fill("hunter2");
  });
  await run(page, "sudo");
  await expect(screen(page)).toContainText("[sudo] password for me:");
  // A typed password would end the prompt and bring the next one.
  await page.waitForTimeout(800);
  await expect(screen(page)).not.toContainText("$ echo");
  const rows = await screen(page).innerText();
  expect(rows.trimEnd().endsWith("[sudo] password for me:")).toBe(true);
});
