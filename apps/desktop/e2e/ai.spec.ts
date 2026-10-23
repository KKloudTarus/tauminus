// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The AI assistant against the fake in `src/ipc/fake-ai.ts`: it answers
// from a script, and lets a local pane's output go only with the setting
// on. Which output may leave is decided and tested in the backend.

import { type Page, expect, test } from "@playwright/test";

const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');
const dialog = (page: Page, name: string | RegExp) => page.getByRole("dialog", { name });

async function palette(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

/** Turns the assistant on from the dialog it opened to, off at first. */
async function turnOn(page: Page, title: string, options: { localOutput?: boolean } = {}) {
  const settings = dialog(page, "AI assistant settings");
  await expect(settings).toBeVisible();
  await settings.getByLabel("Turn on the AI assistant").check();
  await settings.getByLabel("Server (OpenAI-compatible)").fill("https://api.example/v1");
  await settings.getByLabel("Model").fill("m");
  if (options.localOutput) {
    await settings.getByLabel("Local terminals may send their output").check();
  }
  await settings.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog(page, title)).toBeVisible();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("is off until turned on, and refuses cleartext to another machine", async ({ page }) => {
  await palette(page, "Ask AI for a command");
  const settings = dialog(page, "AI assistant settings");
  await settings.getByLabel("Turn on the AI assistant").check();
  await settings.getByLabel("Server (OpenAI-compatible)").fill("http://10.0.0.5:11434/v1");
  await settings.getByLabel("Model").fill("llama3");
  await settings.getByRole("button", { name: "Save", exact: true }).click();
  await expect(settings.getByRole("alert")).toContainText("only for this computer");
  await expect(settings).toBeVisible();
});

test("puts a suggested command on the line without running it", async ({ page }) => {
  await palette(page, "Ask AI for a command");
  await turnOn(page, "Ask AI for a command");
  const ask = dialog(page, "Ask AI for a command");
  await ask.getByLabel("What should the command do?").fill("list the files");
  await ask.getByRole("button", { name: "Ask" }).click();
  await expect(ask.getByTestId("ai-command")).toHaveText("ls -la");
  await expect(ask.getByTestId("ai-answer")).toContainText("Lists the files.");
  await ask.getByRole("button", { name: "Put on the line" }).click();
  await expect(ask).toHaveCount(0);
  await expect(screen(page)).toContainText("$ ls -la");
  // Enter is the user's: nothing ran yet, so no new prompt.
  await expect(screen(page).locator("div").filter({ hasText: /^\$/u })).toHaveCount(1);
});

test("shows the prod guard's reasons before a dangerous command", async ({ page }) => {
  await palette(page, "Ask AI for a command");
  await turnOn(page, "Ask AI for a command");
  const ask = dialog(page, "Ask AI for a command");
  await ask.getByLabel("What should the command do?").fill("delete the data");
  await ask.getByRole("button", { name: "Ask" }).click();
  await expect(ask.getByRole("alert")).toContainText("deletes files recursively");
  await expect(ask.getByRole("button", { name: "Put on the line anyway" })).toBeVisible();
  await ask.getByRole("button", { name: "Copy" }).click();
});

test("explains output only where it may be read, after showing what goes", async ({ page }) => {
  await palette(page, "Explain this pane's output with AI");
  await turnOn(page, "Explain the output with AI");
  const explain = dialog(page, "Explain the output with AI");
  await expect(explain.getByRole("alert")).toContainText("does not let the AI read its output");
  await expect(explain.getByTestId("ai-preview")).toHaveCount(0);
  await explain.getByRole("button", { name: "Close" }).click();

  await palette(page, "AI assistant settings");
  const settings = dialog(page, "AI assistant settings");
  await settings.getByLabel("Local terminals may send their output").check();
  await settings.getByRole("button", { name: "Save", exact: true }).click();
  await expect(settings.getByRole("status")).toHaveText("Saved.");
  await settings.getByRole("button", { name: "Close" }).click();

  await palette(page, "Explain this pane's output with AI");
  await expect(explain.getByTestId("ai-preview")).toContainText("Error 2");
  await expect(explain.getByTestId("ai-preview")).toContainText("KEY=•••");
  await explain.getByRole("button", { name: "Send" }).click();
  await expect(explain.getByTestId("ai-answer")).toContainText("The build failed.");
  await expect(explain.getByTestId("ai-command")).toHaveText("make clean");
});

test("keeps the API key write-only, in the vault", async ({ page }) => {
  await palette(page, "AI assistant settings");
  const settings = dialog(page, "AI assistant settings");
  await settings.getByLabel("Server (OpenAI-compatible)").fill("https://API.Example/v1/");
  await settings.getByLabel("Model").fill("m");
  await settings.getByRole("button", { name: "Save", exact: true }).click();
  await expect(settings.getByText("No key saved.")).toBeVisible();
  // The form shows the address as saved, so the key may go with it.
  await expect(settings.getByLabel("Server (OpenAI-compatible)")).toHaveValue(
    "https://api.example/v1",
  );
  await settings.getByLabel("API key").fill("sk-test");
  await settings.getByRole("button", { name: "Save key" }).click();
  // The vault does not exist yet: it asks for one first.
  await expect(page.getByRole("dialog", { name: "Vault" })).toBeVisible();
});
