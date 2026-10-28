// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// SSH flows against the fake hosts in `src/ipc/fake.ts`: `prompt.test` asks
// to trust its key and for the password `secret`, `kbd.test` asks for the
// code 123456, `changed.test` has a changed key, and others do not resolve.

import { type Page, expect, test } from "@playwright/test";

const focusedPane = (page: Page) => page.locator('section[data-pane-id][data-focused="true"]');
const screenOf = (page: Page) => focusedPane(page).locator(".xterm-rows");
const tabs = (page: Page) => page.getByRole("tab");

async function connect(page: Page, destination: string, jumps = "") {
  await page.keyboard.press("Control+Shift+O");
  const dialog = page.getByRole("dialog", { name: "Connect to SSH host" });
  await dialog.getByLabel("Destination").fill(destination);
  if (jumps) await dialog.getByLabel(/Jump hosts/).fill(jumps);
  await dialog.getByRole("button", { name: "Connect" }).click();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screenOf(page)).toContainText("$");
});

test("trusts a new host, signs in with a password, and runs a command", async ({ page }) => {
  await connect(page, "alice@prompt.test");
  await expect(tabs(page)).toHaveCount(2);

  const hostKey = page.getByRole("dialog", { name: "New host: prompt.test" });
  await expect(hostKey.getByTestId("host-key-fingerprint")).toHaveText(/^SHA256:/);
  await expect(hostKey.getByRole("button", { name: "Cancel" })).toBeFocused();
  await hostKey.getByRole("button", { name: "Trust and remember" }).click();

  const password = page.getByRole("dialog", { name: "Password for alice@prompt.test" });
  await password.getByLabel("Password").fill("wrong");
  await password.getByLabel("Password").press("Enter");
  // Asked again, with the field cleared.
  await expect(password.getByLabel("Password")).toHaveValue("");
  await password.getByLabel("Password").fill("secret");
  await password.getByRole("button", { name: "Continue" }).click();

  await expect(screenOf(page)).toContainText("Welcome to prompt.test");
  await expect(tabs(page).nth(1)).toContainText("alice@prompt.test");
  await page.keyboard.type("echo over ssh");
  await page.keyboard.press("Enter");
  await expect(screenOf(page)).toContainText("over ssh");
});

test("answers a keyboard-interactive challenge", async ({ page }) => {
  await connect(page, "bob@kbd.test:2222");
  await page.getByRole("button", { name: "Trust once" }).click();
  const challenge = page.getByRole("dialog", { name: "Two-factor" });
  await expect(challenge).toContainText("Enter the code from your app");
  await challenge.getByLabel("Code:").fill("123456");
  await challenge.getByLabel("Code:").press("Enter");
  await expect(screenOf(page)).toContainText("Welcome to kbd.test");
});

test("blocks a changed host key and offers no way around it", async ({ page }) => {
  await connect(page, "alice@changed.test");
  const alert = page.getByRole("alert");
  await expect(alert.getByRole("heading")).toHaveText(
    "Warning: the host key for alice@changed.test has changed",
  );
  await expect(alert).toContainText("known_hosts:3");
  await expect(alert.getByRole("button", { name: "Try again" })).toHaveCount(0);
});

test("cancelling the host key prompt ends the attempt and allows a retry", async ({ page }) => {
  await connect(page, "alice@prompt.test");
  await page.getByRole("dialog", { name: "New host: prompt.test" }).press("Escape");
  const alert = page.getByRole("alert");
  await expect(alert.getByRole("heading")).toHaveText("Connection to alice@prompt.test cancelled");
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("dialog", { name: "New host: prompt.test" })).toBeVisible();
});

test("reports an unreachable host with next steps", async ({ page }) => {
  await connect(page, "nobody.test");
  const alert = page.getByRole("alert");
  await expect(alert.getByRole("heading")).toHaveText("Could not reach nobody.test");
  await expect(alert).toContainText("Check the host name and port");
});

test("says when a password manager gave no secret, and how to sign in to it", async ({ page }) => {
  await connect(page, "locked-manager.test");
  const alert = page.getByRole("alert");
  await expect(alert.getByRole("heading")).toHaveText(
    "Could not read the secret to sign in to locked-manager.test",
  );
  await expect(alert).toContainText("reads it from a password manager");
  await expect(alert).toContainText("run vault login");
  await expect(alert).not.toContainText("ssh-agent");
  await expect(alert.getByRole("button", { name: "Try again" })).toBeVisible();
});

test("validates the destination before connecting", async ({ page }) => {
  await page.keyboard.press("Control+Shift+O");
  const dialog = page.getByRole("dialog", { name: "Connect to SSH host" });
  await dialog.getByLabel("Destination").fill("alice@bad host");
  await dialog.getByRole("button", { name: "Connect" }).click();
  await expect(dialog.getByRole("alert")).toHaveText('"bad host" is not a valid host name');
  await dialog.getByLabel("Destination").press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(tabs(page)).toHaveCount(1);
});

test("a split of an SSH pane connects to the same host", async ({ page }) => {
  await connect(page, "bob@kbd.test");
  await page.getByRole("button", { name: "Trust once" }).click();
  await page.getByLabel("Code:").fill("123456");
  await page.getByLabel("Code:").press("Enter");
  await expect(screenOf(page)).toContainText("Welcome to kbd.test");
  await page.keyboard.press("Control+Shift+D");
  await expect(page.getByRole("dialog", { name: "New host: kbd.test" })).toBeVisible();
});
