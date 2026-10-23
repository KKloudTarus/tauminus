// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Snippets: saved in their dialog, run from the palette with their
// placeholders filled in, and typed into each new shell on a host that
// starts with them, against `src/ipc/fake-snippets.ts`.

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

async function newSnippet(page: Page, name: string, body: string, folder = "") {
  await palette(page, "Snippets");
  const dialog = page.getByRole("dialog", { name: "Snippets" });
  await dialog.getByRole("button", { name: "New snippet" }).click();
  const form = dialog.getByRole("form", { name: "New snippet" });
  await form.getByLabel("Name").fill(name);
  if (folder) await form.getByLabel("Folder").fill(folder);
  await form.getByLabel("Command").fill(body);
  await form.getByRole("button", { name: "Save" }).click();
  await expect(form).toHaveCount(0);
  await expect(dialog.getByRole("listitem").filter({ hasText: name })).toBeVisible();
  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toHaveCount(0);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
});

test("a snippet runs from the palette after its placeholders are filled in", async ({ page }) => {
  await newSnippet(page, "greet", "echo hello {{who:world}}", "Demo");
  await palette(page, "Run snippet: Demo / greet");
  const form = page.getByRole("dialog", { name: "Run Demo / greet" });
  await expect(form.getByLabel("who")).toHaveValue("world");
  await form.getByLabel("who").fill("tauminus");
  await form.getByRole("button", { name: "Run" }).click();
  await expect(focusedScreen(page)).toContainText("hello tauminus");
});

test("one without placeholders runs at once, and edits and deletes stick", async ({ page }) => {
  await newSnippet(page, "stamp", "echo stamped");
  await palette(page, "Run snippet: stamp");
  await expect(focusedScreen(page)).toContainText("stamped");

  await palette(page, "Snippets");
  const dialog = page.getByRole("dialog", { name: "Snippets" });
  await dialog.getByRole("button", { name: "Edit stamp" }).click();
  await dialog.getByLabel("Command").fill("echo restamped");
  await dialog.getByRole("button", { name: "Save" }).click();
  await dialog.getByRole("button", { name: "Run stamp", exact: true }).click();
  await expect(focusedScreen(page)).toContainText("restamped");

  await palette(page, "Snippets");
  await dialog.getByRole("button", { name: "Delete stamp" }).click();
  await expect(dialog).toContainText("No snippets yet.");
});

test("a host types its startup snippets into each new shell", async ({ page }) => {
  await newSnippet(page, "hello", "echo started on {{where:prompt}}");
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("db");
  await editor.getByLabel("Address").fill("prompt.test");
  await editor.getByLabel("User (blank inherits)").fill("ops");
  await editor.getByRole("combobox", { name: "Add" }).selectOption({ label: "hello" });
  await expect(editor.getByRole("list", { name: "Startup snippets" })).toContainText("hello");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);

  await sidebar(page)
    .getByRole("button", { name: /^db ops@prompt\.test/ })
    .click();
  await page.getByRole("button", { name: "Trust once" }).click();
  const password = page.getByRole("dialog", { name: "Password for ops@prompt.test" });
  await password.getByLabel("Password", { exact: true }).fill("secret");
  await password.getByLabel("Password", { exact: true }).press("Enter");
  await expect(focusedScreen(page)).toContainText("Welcome to prompt.test");
  await expect(focusedScreen(page)).toContainText("started on prompt");
});
