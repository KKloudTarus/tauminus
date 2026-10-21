// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Saved hosts against the in-memory store in `src/ipc/fake-hosts.ts`. Its
// ~/.ssh/config names prompt.test, kbd.test and changed.test, the fake SSH
// hosts in `src/ipc/fake.ts`.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const focusedScreen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

async function importConfig(page: Page) {
  await sidebar(page).getByRole("button", { name: "Import 3 hosts from ~/.ssh/config" }).click();
  await expect(sidebar(page).getByRole("status")).toHaveText(/Imported 3 hosts/);
}

async function newHost(
  page: Page,
  fields: { name: string; address: string; user?: string; group?: string },
) {
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const dialog = page.getByRole("dialog", { name: "New host" });
  await dialog.getByLabel("Name").fill(fields.name);
  await dialog.getByLabel("Address").fill(fields.address);
  if (fields.user) await dialog.getByLabel("User (blank inherits)").fill(fields.user);
  if (fields.group)
    await dialog.getByRole("combobox", { name: "Group" }).selectOption({ label: fields.group });
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(dialog).toHaveCount(0);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
});

test("offers to import ~/.ssh/config on first run", async ({ page }) => {
  await expect(sidebar(page)).toContainText("No saved hosts yet.");
  await importConfig(page);
  const tree = sidebar(page).getByRole("list", { name: "Saved hosts" });
  await expect(tree.getByRole("button", { name: "SSH config", exact: true })).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await Promise.all(
    ["prompt", "kbd", "changed"].map((name) =>
      expect(tree.getByRole("button", { name: new RegExp(`^${name}`) })).toBeVisible(),
    ),
  );
  await expect(sidebar(page)).not.toContainText("No saved hosts yet.");
});

test("connects to a saved host through its prompts", async ({ page }) => {
  await importConfig(page);
  await sidebar(page)
    .getByRole("button", { name: /^prompt alice@prompt\.test/ })
    .click();
  await page.getByRole("button", { name: "Trust once" }).click();
  const password = page.getByRole("dialog", { name: "Password for alice@prompt.test" });
  await password.getByLabel("Password", { exact: true }).fill("secret");
  await password.getByLabel("Password", { exact: true }).press("Enter");
  await expect(focusedScreen(page)).toContainText("Welcome to prompt.test");
  await expect(page.getByRole("tab", { name: /^prompt/ })).toHaveAttribute("aria-selected", "true");
});

test("searches by any field and connects to the best match with Enter", async ({ page }) => {
  await importConfig(page);
  const search = sidebar(page).getByRole("searchbox", { name: "Search hosts" });
  await search.fill("bob");
  const results = sidebar(page).getByRole("list", { name: "Search results" });
  await expect(results.getByRole("listitem")).toHaveCount(1);
  await expect(results).toContainText("kbd");
  await search.press("Enter");
  await expect(page.getByRole("dialog", { name: "New host: kbd.test" })).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();

  await search.fill("nothing-like-this");
  await expect(sidebar(page)).toContainText("No host matches.");
  await search.press("Escape");
  await expect(sidebar(page).getByRole("list", { name: "Saved hosts" })).toBeVisible();
});

test("adds and edits a host", async ({ page }) => {
  await newHost(page, { name: "web", address: "web.internal", user: "deploy" });
  const row = sidebar(page).getByRole("button", { name: /^web deploy@web\.internal/ });
  await expect(row).toBeVisible();

  await sidebar(page).getByRole("button", { name: "Edit web" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit web" });
  await dialog.getByLabel("Port").fill("2222");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(
    sidebar(page).getByRole("button", { name: /^web deploy@web\.internal:2222/ }),
  ).toBeVisible();
});

test("a host inherits its group's user until it sets its own", async ({ page }) => {
  await sidebar(page).getByRole("button", { name: "New group" }).click();
  const groupDialog = page.getByRole("dialog", { name: "New group" });
  await groupDialog.getByLabel("Name").fill("prod");
  await groupDialog.getByLabel("User").fill("ops");
  await groupDialog.getByRole("button", { name: "Save" }).click();

  await newHost(page, { name: "db", address: "db.internal", group: "prod" });
  await expect(sidebar(page).getByRole("button", { name: /^db ops@db\.internal/ })).toBeVisible();

  // Removing the group keeps the host, now at the top level.
  await sidebar(page).getByRole("button", { name: "Edit group prod" }).click();
  const edit = page.getByRole("dialog", { name: "Edit group prod" });
  await edit.getByRole("button", { name: "Delete" }).click();
  await expect(edit).toBeVisible();
  await edit.getByRole("button", { name: "Confirm delete" }).click();
  await expect(sidebar(page).getByRole("button", { name: /^db me@db\.internal/ })).toBeVisible();
});

test("deleting a host takes a second click", async ({ page }) => {
  await newHost(page, { name: "temp", address: "temp.internal" });
  await sidebar(page).getByRole("button", { name: "Edit temp" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit temp" });
  await dialog.getByRole("button", { name: "Delete" }).click();
  await expect(dialog.getByRole("button", { name: "Confirm delete" })).toBeVisible();
  await dialog.getByRole("button", { name: "Confirm delete" }).click();
  await expect(sidebar(page).getByRole("button", { name: /^temp/ })).toHaveCount(0);
});

test("the sidebar toggles and remembers its state", async ({ page }) => {
  await page.locator('section[data-pane-id][data-focused="true"]').click();
  await page.keyboard.press("Control+Shift+H");
  await expect(sidebar(page)).toHaveCount(0);
  await page.reload();
  await expect(focusedScreen(page)).toContainText("$");
  await expect(sidebar(page)).toHaveCount(0);
  await page.keyboard.press("Control+Shift+H");
  await expect(sidebar(page)).toBeVisible();
});
