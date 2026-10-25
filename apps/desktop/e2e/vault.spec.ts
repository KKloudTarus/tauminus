// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The vault against the in-memory one in `src/ipc/fake-vault.ts`.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const vault = (page: Page) => page.getByRole("dialog", { name: "Vault" });

async function createVault(page: Page) {
  await sidebar(page).getByRole("button", { name: "Vault", exact: true }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct horse");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await expect(vault(page).getByRole("heading", { name: "Identities" })).toBeVisible();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".xterm-rows").first()).toContainText("$");
});

test("refuses a short or mistyped master password", async ({ page }) => {
  await sidebar(page).getByRole("button", { name: "Vault", exact: true }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct hose");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await expect(vault(page).getByRole("alert")).toHaveText("The passwords do not match");
  await vault(page).getByLabel("Master password").fill("short");
  await vault(page).getByLabel("Repeat it").fill("short");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await expect(vault(page).getByRole("alert")).toContainText("8 to 1024 characters");
});

test("stores an identity, locks, and unlocks again", async ({ page }) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Deploy");
  await form.getByLabel("User").fill("deploy");
  await form.getByLabel("Password", { exact: true }).fill("hunter2");
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await expect(list).toContainText("deploy · password");
  // A stored secret never comes back to the view.
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  await expect(vault(page).getByLabel("Password (stored; type to replace)")).toHaveValue("");
  await vault(page).getByLabel("Remove the stored password").check();
  await vault(page).getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("deploy");
  await expect(list).not.toContainText("password");

  await vault(page).getByRole("button", { name: "Lock now" }).click();
  await vault(page).getByLabel("Master password").fill("wrong password");
  await vault(page).getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(vault(page).getByRole("alert")).toHaveText("the password is wrong");
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(list).toContainText("Deploy");
});

test("a host picks an identity, and keeps it while the vault is locked", async ({ page }) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  await vault(page).getByLabel("Name").fill("Deploy");
  await vault(page).getByRole("button", { name: "Save" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();

  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("web");
  await editor.getByLabel("Address").fill("web.test");
  await editor.getByLabel("Identity from the vault").selectOption({ label: "Deploy" });
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);

  await page.keyboard.press("Control+Shift+P");
  await page.keyboard.type("lock the vault");
  await page.keyboard.press("Enter");
  await sidebar(page).getByRole("button", { name: "Edit web", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Edit web" }).getByLabel("Identity from the vault"),
  ).toHaveValue("identity1");
});

test("a host that needs the vault unlocks it and connects", async ({ page }) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "Lock now" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("vaulted");
  await editor.getByLabel("Address").fill("vault.test");
  await editor.getByRole("button", { name: "Save" }).click();
  await sidebar(page)
    .getByRole("button", { name: /^vaulted/ })
    .click();
  const pane = page.locator('section[data-pane-id][data-focused="true"]');
  await expect(pane.getByRole("alert")).toContainText("Unlock the vault to sign in to vaulted");
  await pane.getByRole("button", { name: "Unlock the vault" }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(vault(page)).toHaveCount(0);
  await expect(pane.locator(".xterm-rows")).toContainText("signed in from the vault");
});

test("turning the keychain on asks for the master password", async ({ page }) => {
  await createVault(page);
  await vault(page).getByText("Settings").click();
  await vault(page).getByLabel("Let the system keychain unlock the vault").check();
  const confirm = vault(page).getByLabel("Master password, to turn this on");
  await confirm.fill("wrong password");
  await vault(page).getByRole("button", { name: "Turn on" }).click();
  await expect(vault(page).getByRole("alert")).toHaveText("the password is wrong");
  // A refused attempt leaves the switch off; turning it on asks again.
  await expect(
    vault(page).getByLabel("Let the system keychain unlock the vault"),
  ).not.toBeChecked();
  await vault(page).getByLabel("Let the system keychain unlock the vault").click();
  await confirm.fill("correct horse");
  await vault(page).getByRole("button", { name: "Turn on" }).click();
  await expect(vault(page).getByLabel("Let the system keychain unlock the vault")).toBeChecked();
  await vault(page).getByRole("button", { name: "Lock now" }).click();
  await vault(page).getByRole("button", { name: "Unlock with the system keychain" }).click();
  await expect(vault(page).getByRole("heading", { name: "Identities" })).toBeVisible();
});

test("a password change clears the error of a failed one", async ({ page }) => {
  await createVault(page);
  await vault(page).getByText("Settings").click();
  await vault(page).getByLabel("Current master password").fill("wrong password");
  await vault(page).getByLabel("New master password").fill("battery staple");
  await vault(page).getByRole("button", { name: "Change password" }).click();
  await expect(vault(page).getByRole("alert")).toHaveText("the password is wrong");
  await vault(page).getByLabel("Current master password").fill("correct horse");
  await vault(page).getByLabel("New master password").fill("battery staple");
  await vault(page).getByRole("button", { name: "Change password" }).click();
  await expect(vault(page).getByText("The master password changed.")).toBeVisible();
  await expect(vault(page).getByRole("alert")).toHaveCount(0);
});

test("generates a key, shows its public half, and installs it on a host", async ({ page }) => {
  await sidebar(page).getByRole("button", { name: "New host" }).click();
  const editor = page.getByRole("dialog", { name: "New host" });
  await editor.getByLabel("Name").fill("web");
  await editor.getByLabel("Address").fill("web.test");
  await editor.getByRole("button", { name: "Save" }).click();
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Deploy");
  await form.getByRole("combobox", { name: /^Key/ }).selectOption("ed25519");
  await expect(form.getByLabel("Private key (OpenSSH)")).toHaveCount(0);
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await expect(list).toContainText("key");
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  await expect(vault(page).getByText("ssh-ed25519 AAAAgenerated tauminus-Deploy")).toBeVisible();
  await vault(page).getByLabel("Host to install the key on").selectOption({ label: "web" });
  await vault(page).getByRole("button", { name: "Install on host" }).click();
  await vault(page).getByRole("button", { name: "Trust once" }).click();
  // Enter in the prompt answers it; it must not submit the identity form.
  await vault(page).getByLabel("Password", { exact: true }).last().fill("hunter2");
  await page.keyboard.press("Enter");
  await expect(vault(page).getByText("The key is in authorized_keys on web.")).toBeVisible();
});

test("removing the key wins over a generate choice", async ({ page }) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Deploy");
  await form.getByRole("combobox", { name: /^Key/ }).selectOption("ed25519");
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  const edit = vault(page).getByRole("form", { name: "Edit Deploy" });
  await edit.getByRole("combobox", { name: /^Key/ }).selectOption("rsa3072");
  await edit.getByLabel("Remove the stored key and its passphrase").check();
  await edit.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("nothing stored");
});

test("the keychain can ask for Touch ID first, and says what that guards", async ({ page }) => {
  await createVault(page);
  await vault(page).getByText("Settings").click();
  const ask = vault(page).getByLabel("Ask for Touch ID before the keychain unlocks the vault");
  // Only once the keychain unlocks the vault.
  await expect(ask).toHaveCount(0);
  await vault(page).getByLabel("Let the system keychain unlock the vault").check();
  await vault(page).getByLabel("Master password, to turn this on").fill("correct horse");
  await vault(page).getByRole("button", { name: "Turn on" }).click();
  await ask.check();
  await expect(ask).toBeChecked();
  await expect(vault(page)).toContainText("Other programs running as you can still read that key.");
  await vault(page).getByRole("button", { name: "Lock now" }).click();
  await vault(page).getByRole("button", { name: "Unlock with Touch ID" }).click();
  await expect(vault(page).getByRole("heading", { name: "Identities" })).toBeVisible();
});

test("an identity is kept here unless shared through sync", async ({ page }) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Deploy");
  await expect(form).toContainText("Kept on this device only");
  await form.getByLabel("Share through sync").check();
  await expect(form).toContainText("Stopping later does not take them back");
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await expect(list).toContainText("shared through sync");
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  await expect(vault(page).getByLabel("Share through sync")).toBeChecked();
});
