// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Files on a saved host against the in-memory sides in
// `src/ipc/fake-files.ts`: home is /home/me here and /home/ops there.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const focusedScreen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');
const view = (page: Page) => page.getByRole("dialog", { name: "Files on db" });
const remote = (page: Page) => view(page).getByRole("list", { name: "db entries" });
const local = (page: Page) => view(page).getByRole("list", { name: "This computer entries" });

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const dialog = page.getByRole("dialog", { name: "New host" });
  await dialog.getByLabel("Name").fill("db");
  await dialog.getByLabel("Address").fill("db.internal");
  await dialog.getByRole("button", { name: "Save" }).click();
  await sidebar(page).getByRole("button", { name: "Files on db" }).click();
  await expect(remote(page)).toContainText("app.log");
  await expect(local(page)).toContainText("notes.txt");
});

test("downloads a file and lists it here", async ({ page }) => {
  await remote(page)
    .getByRole("button", { name: /app\.log/ })
    .click();
  await view(page).getByRole("button", { name: "← Download" }).click();
  const transfers = view(page).getByRole("list", { name: "Transfers" });
  await expect(transfers).toContainText("app.log");
  await expect(transfers).toContainText("done");
  await expect(local(page)).toContainText("app.log");
});

test("goes into folders and back up", async ({ page }) => {
  await remote(page)
    .getByRole("button", { name: /deploy/ })
    .dblclick();
  await expect(remote(page)).toContainText("run.sh");
  await expect(view(page).getByLabel("db folder")).toHaveValue("/home/ops/deploy");
  await view(page).getByRole("button", { name: "Up from /home/ops/deploy" }).click();
  await expect(remote(page)).toContainText("app.log");
});

test("files dropped from the OS go to the remote folder", async ({ page }) => {
  await page.evaluate(() => window.tauminusTestDrop?.(["/home/me/notes.txt"]));
  await expect(view(page).getByRole("list", { name: "Transfers" })).toContainText("done");
  await expect(remote(page)).toContainText("notes.txt");
});

test("makes, renames, changes, and deletes on the server", async ({ page }) => {
  await view(page).getByRole("button", { name: "New folder" }).click();
  await view(page).getByLabel("New folder name").fill("releases");
  await view(page).getByLabel("New folder name").press("Enter");
  await expect(remote(page)).toContainText("releases");

  await remote(page)
    .getByRole("button", { name: /releases/ })
    .click();
  await view(page).getByRole("button", { name: "Rename" }).click();
  await view(page).getByLabel("New name for releases").fill("builds");
  await view(page).getByLabel("New name for releases").press("Enter");
  await expect(remote(page)).toContainText("builds");

  await remote(page)
    .getByRole("button", { name: /app\.log/ })
    .click();
  await view(page).getByRole("button", { name: "Permissions" }).click();
  await view(page).getByLabel("Permissions of app.log").fill("9");
  await view(page).getByLabel("Permissions of app.log").press("Enter");
  await expect(view(page)).toContainText("Write the mode in octal");
  await view(page).getByLabel("Permissions of app.log").fill("600");
  await view(page).getByLabel("Permissions of app.log").press("Enter");
  await expect(remote(page).getByRole("button", { name: /app\.log/ })).toContainText("rw-------");

  await view(page).getByRole("button", { name: "Delete" }).click();
  await expect(remote(page)).not.toContainText("app.log");
  await page.keyboard.press("Escape");
  await expect(view(page)).toHaveCount(0);
});

test("opens a remote file for editing until it is done", async ({ page }) => {
  await remote(page)
    .getByRole("button", { name: /app\.log/ })
    .click();
  await view(page).getByRole("button", { name: "Edit" }).click();
  const editing = view(page).getByRole("list", { name: "Editing" });
  await expect(editing).toContainText("/home/ops/app.log");
  await expect(editing).toContainText("each save goes to the server");
  await editing.getByRole("button", { name: "Done" }).click();
  await expect(editing).toHaveCount(0);
});
