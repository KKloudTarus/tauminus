// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Forwarding rules: saved in the host editor, started and stopped from the
// palette, against the in-memory backend in `src/ipc/fake-forwards.ts`.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const focusedScreen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

async function palette(page: Page, command: string) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const dialog = page.getByRole("dialog", { name: "New host" });
  await dialog.getByLabel("Name").fill("db");
  await dialog.getByLabel("Address").fill("db.internal");
  await dialog.getByText("Port forwarding").click();
  await dialog.getByRole("button", { name: "Add Local (-L)" }).click();
  // Named "New rule" until it has a name, then by it.
  const rule = dialog.locator("fieldset").nth(0);
  await rule.getByLabel("Rule name").fill("postgres");
  await rule.getByLabel("Listen port").fill("15432");
  await rule.getByLabel("Destination port").fill("5432");
  await dialog.getByRole("button", { name: "Add SOCKS (-D)" }).click();
  await dialog.locator("fieldset").nth(1).getByLabel("Rule name").fill("busy");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(dialog).toHaveCount(0);
});

test("a saved rule starts and stops from the palette", async ({ page }) => {
  await palette(page, "Start forward: postgres");
  const list = page.getByRole("dialog", { name: "Port forwarding" });
  const postgres = list.getByRole("listitem").filter({ hasText: "postgres" });
  await expect(postgres).toContainText("localhost:41000 → localhost:5432 (from the server)");
  await expect(postgres).toContainText("Listening on 41000");
  await postgres.getByRole("button", { name: "Stop" }).click();
  await expect(postgres).toContainText("Stopped");
  await page.keyboard.press("Escape");
  await expect(list).toHaveCount(0);

  await palette(page, "Start forward: postgres");
  await expect(
    page.getByRole("dialog", { name: "Port forwarding" }).getByRole("listitem").first(),
  ).toContainText("Listening on");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Control+Shift+P");
  await expect(page.getByRole("option", { name: "Stop forward: postgres (db)" })).toBeVisible();
});

test("a rule that cannot start says why", async ({ page }) => {
  await palette(page, "Port forwarding");
  const busy = page
    .getByRole("dialog", { name: "Port forwarding" })
    .getByRole("listitem")
    .filter({ hasText: "busy" });
  await expect(busy).toContainText("SOCKS proxy on localhost:1080");
  await busy.getByRole("button", { name: "Start" }).click();
  await expect(busy).toContainText("address in use");
  await expect(busy.getByRole("button", { name: "Start" })).toBeEnabled();
});

test("the editor keeps rules and warns about a listener open to the network", async ({ page }) => {
  await sidebar(page).getByRole("button", { name: "Edit db" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit db" });
  const postgres = dialog.getByRole("group", { name: "postgres" });
  await expect(postgres.getByLabel("Listen port")).toHaveValue("15432");
  await postgres.getByLabel("Listen on this machine").selectOption("any");
  await expect(postgres).toContainText("Other machines on the network can use this tunnel.");
  await postgres.getByLabel("Destination port").fill("0");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(dialog).toContainText("postgres: the destination port must be 1 to 65535");
});
