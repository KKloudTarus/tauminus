// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { expect, test } from "@playwright/test";

test("offers a new version at start and installs it when asked", async ({ page }) => {
  await page.addInitScript(() => window.localStorage.setItem("tauminus.e2e.update", "1"));
  await page.goto("/");
  const banner = page.getByRole("status", { name: "Updates" });
  await expect(banner).toContainText("Tauminus 9.9.9 is available; this is 0.0.0.");
  await banner.getByText("What changed").click();
  await expect(banner).toContainText("Fewer bugs.");
  await banner.getByRole("button", { name: "Install and restart" }).click();
  await expect(banner).toContainText("Installing Tauminus 9.9.9");
  expect(await page.evaluate(() => window.localStorage.getItem("tauminus.e2e.installed"))).toBe(
    "9.9.9",
  );
});

test("says nothing at start without a new version, and answers when asked", async ({ page }) => {
  await page.goto("/");
  const banner = page.getByRole("status", { name: "Updates" });
  await expect(page.getByRole("tab").first()).toBeVisible();
  await expect(banner).toHaveCount(0);
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill("Check for updates");
  await input.press("Enter");
  await expect(banner).toHaveText("This is the newest version of Tauminus.");

  await page.keyboard.press("Control+Shift+P");
  await input.fill("Check for updates at start");
  await input.press("Enter");
  expect(await page.evaluate(() => window.localStorage.getItem("tauminus.updates.auto"))).toBe("0");
});
