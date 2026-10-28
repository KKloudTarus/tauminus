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
  await form.getByRole("combobox", { name: /^Key( \(one is stored\))?$/ }).selectOption("ed25519");
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
  await form.getByRole("combobox", { name: /^Key( \(one is stored\))?$/ }).selectOption("ed25519");
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  const edit = vault(page).getByRole("form", { name: "Edit Deploy" });
  await edit.getByRole("combobox", { name: /^Key( \(one is stored\))?$/ }).selectOption("rsa3072");
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

test("a password from 1Password replaces the stored one and keeps the identity here", async ({
  page,
}) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Deploy");
  await form.getByLabel("Password", { exact: true }).fill("hunter2");
  await form.getByLabel("Share through sync").check();
  await form.getByLabel("Password from").selectOption("1Password");
  // The vault's own field goes; the manager's takes its place.
  await expect(form.getByLabel("Password", { exact: true })).toHaveCount(0);
  await expect(form).toContainText("asks you, in a system dialog");
  await expect(form.getByLabel("Share through sync")).toBeDisabled();
  await expect(form.getByLabel("Share through sync")).not.toBeChecked();
  await expect(form).toContainText("It reads a password manager, so it stays on this device.");
  // An empty reference does not save.
  await form.getByRole("button", { name: "Save" }).click();
  await expect(form).toBeVisible();
  await form.getByLabel("Secret reference").fill("op://Ops/db/password");
  await form.getByRole("button", { name: "Save" }).click();

  const list = vault(page).getByRole("list", { name: "Identities" });
  await expect(list).toContainText("password from 1Password");
  await expect(list).not.toContainText("shared through sync");
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  const edit = vault(page).getByRole("form", { name: "Edit Deploy" });
  await expect(edit.getByLabel("Password from")).toHaveValue("onePassword");
  await expect(edit.getByLabel("Secret reference")).toHaveValue("op://Ops/db/password");
  await expect(edit).not.toContainText("asks you, in a system dialog");

  // Back to this vault: the reference goes, nothing is stored, and
  // nothing new needs a yes.
  await edit.getByLabel("Password from").selectOption("This vault");
  await expect(edit).not.toContainText("asks you, in a system dialog");
  await expect(edit.getByLabel("Password", { exact: true })).toHaveValue("");
  await expect(edit.getByLabel("Remove the stored password")).toHaveCount(0);
  await edit.getByRole("button", { name: "Save" }).click();
  await expect(list).not.toContainText("password");
});

test("a key comes from 1Password or Vault, and its passphrase from any manager", async ({
  page,
}) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Ops");
  const keyFrom = form.getByLabel("Private key from");
  await expect(keyFrom.getByRole("option")).toHaveText([
    "This vault",
    "1Password",
    "HashiCorp Vault",
  ]);
  await expect(form.getByLabel("Key passphrase from").getByRole("option")).toHaveText([
    "This vault",
    "1Password",
    "Bitwarden",
    "HashiCorp Vault",
  ]);
  await keyFrom.selectOption("HashiCorp Vault");
  await expect(form.getByLabel("Key", { exact: true })).toHaveCount(0);
  const key = form.getByRole("group", { name: "Private key source" });
  await expect(key.getByLabel("KV mount")).toHaveValue("secret");
  await expect(key).toContainText("that token goes to this address");
  await key.getByLabel("Vault address").fill("https://vault.example.com:8200");
  await key.getByLabel("Secret path").fill("ssh/ops");
  await key.getByLabel("Field").fill("private_key");
  // A passphrase from Vault starts at the key's secret, with its own field.
  await form.getByLabel("Key passphrase from").selectOption("HashiCorp Vault");
  const passphrase = form.getByRole("group", { name: "Key passphrase source" });
  await expect(passphrase.getByLabel("Vault address")).toHaveValue(
    "https://vault.example.com:8200",
  );
  await expect(passphrase.getByLabel("Secret path")).toHaveValue("ssh/ops");
  await expect(passphrase.getByLabel("Field")).toHaveValue("");
  await form.getByLabel("Key passphrase from").selectOption("Bitwarden");
  await form
    .getByRole("group", { name: "Key passphrase source" })
    .getByLabel("Item id")
    .fill("2f2a3b4c-0000-4000-8000-000000000001");
  await form.getByRole("button", { name: "Save" }).click();

  const list = vault(page).getByRole("list", { name: "Identities" });
  await expect(list).toContainText("key from HashiCorp Vault");
  await list.getByRole("button", { name: "Edit Ops" }).click();
  const edit = vault(page).getByRole("form", { name: "Edit Ops" });
  await expect(edit.getByLabel("Secret path")).toHaveValue("ssh/ops");
  await expect(edit.getByLabel("Item id")).toHaveValue("2f2a3b4c-0000-4000-8000-000000000001");
});

test("a removed or generated key takes its passphrase reference with it", async ({ page }) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Ops");
  await form.getByLabel("Private key (OpenSSH)").fill("-----BEGIN OPENSSH PRIVATE KEY-----");
  await form.getByLabel("Key passphrase from").selectOption("1Password");
  await form.getByLabel("Secret reference").fill("op://Ops/key/passphrase");
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await expect(list).toContainText("key · passphrase from 1Password");

  // Removing the key removes what unlocked it.
  await list.getByRole("button", { name: "Edit Ops" }).click();
  let edit = vault(page).getByRole("form", { name: "Edit Ops" });
  await expect(edit).toContainText("It reads a password manager");
  await edit.getByLabel("Remove the stored key and its passphrase").check();
  await expect(edit.getByLabel("Share through sync")).toBeEnabled();
  await edit.getByRole("button", { name: "Save" }).click();
  await expect(list).not.toContainText("passphrase");
  await expect(list).not.toContainText("key");

  // A generated key has no passphrase to read.
  await list.getByRole("button", { name: "Edit Ops" }).click();
  edit = vault(page).getByRole("form", { name: "Edit Ops" });
  await edit.getByLabel("Key passphrase from").selectOption("1Password");
  await edit.getByLabel("Secret reference").fill("op://Ops/key/passphrase");
  await edit.getByRole("combobox", { name: /^Key( \(one is stored\))?$/ }).selectOption("ed25519");
  await expect(edit.getByLabel("Key passphrase from")).toHaveCount(0);
  await expect(edit).not.toContainText("asks you, in a system dialog");
  await edit.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("Ops");
  await expect(list).toContainText("key");
  await expect(list).not.toContainText("passphrase");
});

test("a shared identity that starts reading a manager says what the others keep", async ({
  page,
}) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Deploy");
  await form.getByLabel("Password", { exact: true }).fill("hunter2");
  await form.getByLabel("Share through sync").check();
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await list.getByRole("button", { name: "Edit Deploy" }).click();
  const edit = vault(page).getByRole("form", { name: "Edit Deploy" });
  await edit.getByLabel("Password from").selectOption("Bitwarden");
  await expect(edit).toContainText("so it stops syncing. The other devices keep the password");
  await edit.getByLabel("Item id").fill("2f2a3b4c-0000-4000-8000-000000000001");
  await edit.getByRole("button", { name: "Save" }).click();
  await expect(vault(page).getByRole("alert")).toHaveCount(0);
  await expect(list).toContainText("password from Bitwarden");
  await expect(list).not.toContainText("shared through sync");
});

test("a key moved from the vault to a manager keeps the passphrase set beside it", async ({
  page,
}) => {
  await createVault(page);
  await vault(page).getByRole("button", { name: "New identity" }).click();
  const form = vault(page).getByRole("form", { name: "New identity" });
  await form.getByLabel("Name").fill("Ops");
  await form.getByLabel("Private key (OpenSSH)").fill("-----BEGIN OPENSSH PRIVATE KEY-----");
  await form.getByRole("button", { name: "Save" }).click();
  const list = vault(page).getByRole("list", { name: "Identities" });
  await list.getByRole("button", { name: "Edit Ops" }).click();
  const edit = vault(page).getByRole("form", { name: "Edit Ops" });
  await edit.getByLabel("Remove the stored key and its passphrase").check();
  await expect(edit.getByLabel("Key passphrase from")).toHaveCount(0);
  await edit.getByLabel("Private key from").selectOption("1Password");
  await edit
    .getByRole("group", { name: "Private key source" })
    .getByLabel("Secret reference")
    .fill("op://Ops/key/private key");
  await edit.getByLabel("Key passphrase from").selectOption("Bitwarden");
  await edit.getByLabel("Item id").fill("2f2a3b4c-0000-4000-8000-000000000001");
  await edit.getByRole("button", { name: "Save" }).click();
  await expect(list).toContainText("key from 1Password · passphrase from Bitwarden");
});
