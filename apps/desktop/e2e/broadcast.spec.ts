// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Broadcast input: what is typed in one selected pane goes to the others,
// Ctrl+C at once; and a snippet run on several saved hosts, one pane each.

import { type Page, expect, test } from "@playwright/test";

const panes = (page: Page) => page.locator("section[data-pane-id]");
const screenOf = (page: Page, index: number) => panes(page).nth(index).locator(".xterm-rows");
const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });

async function palette(page: Page, command: string) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

async function newHost(page: Page, name: string) {
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill(name);
  await editor.getByLabel("Address").fill("prompt.test");
  await editor.getByLabel("User (blank inherits)").fill(name);
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);
}

async function logIn(page: Page, user: string) {
  await page.getByRole("button", { name: "Trust once" }).first().click();
  const password = page.getByRole("dialog", { name: `Password for ${user}@prompt.test` });
  await password.getByLabel("Password", { exact: true }).fill("secret");
  await password.getByLabel("Password", { exact: true }).press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screenOf(page, 0)).toContainText("$");
});

test("typing in one pane of a broadcast reaches the others, and Ctrl+C at once", async ({
  page,
}) => {
  await palette(page, "Split pane right");
  await expect(panes(page)).toHaveCount(2);
  await expect(screenOf(page, 1)).toContainText("$");
  await palette(page, "Broadcast input to every pane in this tab");
  await expect(page.getByTestId("broadcast-badge")).toHaveCount(2);

  await page.keyboard.type("echo half");
  await page.keyboard.press("Control+C");
  await page.keyboard.type("echo both");
  await page.keyboard.press("Enter");
  // Rows join without breaks: the output, then the next prompt.
  await Promise.all(
    [0, 1].map((index) =>
      expect(screenOf(page, index)).toContainText("echo half^C$ echo bothboth$"),
    ),
  );

  await palette(page, "Stop broadcasting input");
  await expect(page.getByTestId("broadcast-badge")).toHaveCount(0);
  await page.keyboard.type("echo alone");
  await page.keyboard.press("Enter");
  await expect(screenOf(page, 1)).toContainText("alone");
  await expect(screenOf(page, 0)).not.toContainText("alone");
});

test("a password typed at a prompt goes only to panes asking for one", async ({ page }) => {
  await palette(page, "Split pane right");
  await expect(screenOf(page, 1)).toContainText("$");
  await page.keyboard.type("sudo");
  await page.keyboard.press("Enter");
  await expect(screenOf(page, 1)).toContainText("password for me:");
  await palette(page, "Broadcast input to every pane in this tab");
  await expect(page.getByTestId("broadcast-badge")).toHaveCount(2);

  await page.keyboard.type("hunter2");
  await page.keyboard.press("Enter");
  await expect(screenOf(page, 1)).toContainText("sudo: ok");
  await expect(page.getByRole("status").or(page.getByRole("alert")).first()).toContainText(
    "held a secret back",
  );
  await expect(screenOf(page, 0)).not.toContainText("hunter2");

  // Back at a prompt, typing reaches both again.
  await page.keyboard.type("echo after");
  await page.keyboard.press("Enter");
  await Promise.all(
    [0, 1].map((index) => expect(screenOf(page, index)).toContainText("echo afterafter$")),
  );
});

test("a snippet runs on several saved hosts, one pane each", async ({ page }) => {
  await newHost(page, "web1");
  await newHost(page, "web2");
  await palette(page, "Snippets");
  const dialog = page.getByRole("dialog", { name: "Snippets" });
  await dialog.getByRole("button", { name: "New snippet" }).click();
  await dialog.getByLabel("Name").fill("hello");
  await dialog.getByLabel("Command").fill("echo hello from {{who:all}}");
  await dialog.getByRole("button", { name: "Save" }).click();
  await dialog.getByRole("button", { name: "Run hello on hosts" }).click();

  const picker = page.getByRole("dialog", { name: "Run hello on hosts" });
  await picker.getByRole("checkbox", { name: /web1/ }).check();
  await picker.getByRole("checkbox", { name: /web2/ }).check();
  await picker.getByRole("button", { name: "Run on 2 hosts" }).click();
  const form = page.getByRole("dialog", { name: "Run hello" });
  await form.getByLabel("who").fill("both");
  await form.getByRole("button", { name: "Run" }).click();

  await expect(panes(page)).toHaveCount(2);
  await logIn(page, "web1");
  await logIn(page, "web2");
  // Typed after the login, once each: the command, its output, the prompt.
  await Promise.all(
    [0, 1].map((index) =>
      expect(screenOf(page, index)).toContainText(
        "Welcome to prompt.test$ echo hello from bothhello from both$",
      ),
    ),
  );
  await expect(page.getByTestId("broadcast-badge")).toHaveCount(2);
});
