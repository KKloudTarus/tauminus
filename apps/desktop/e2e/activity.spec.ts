// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Status dots: a hidden tab marks output that arrived while it was hidden,
// and showing it clears the mark.

import { expect, test } from "@playwright/test";

test("a hidden tab shows a dot for new output until it is shown", async ({ page }) => {
  await page.goto("/");
  const first = page.getByRole("tab").first();
  await expect(page.locator(".xterm-rows").first()).toContainText("$");
  await page.keyboard.type("later built");
  await page.keyboard.press("Enter");
  await page.getByRole("button", { name: "New tab" }).click();
  await expect(page.getByRole("tab")).toHaveCount(2);

  await expect(first.getByTestId("activity-dot")).toHaveAttribute("title", "New output");
  await expect(page.getByRole("tab").nth(1).getByTestId("activity-dot")).toHaveCount(0);

  await first.click();
  await expect(first.getByTestId("activity-dot")).toHaveCount(0);
});

test("a pane split after the tab opened marks it too", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".xterm-rows").first()).toContainText("$");
  await page.keyboard.press("Control+Shift+P");
  await page.getByPlaceholder("Type a command").fill("Split pane right");
  await page.getByPlaceholder("Type a command").press("Enter");
  await expect(page.locator("section[data-pane-id]")).toHaveCount(2);
  await expect(page.locator(".xterm-rows").nth(1)).toContainText("$");
  await page.keyboard.type("later done");
  await page.keyboard.press("Enter");
  await page.getByRole("button", { name: "New tab" }).click();
  await expect(page.getByRole("tab").first().getByTestId("activity-dot")).toHaveAttribute(
    "title",
    "New output",
  );
});
