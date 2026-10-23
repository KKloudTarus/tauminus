// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Serial ports: listed, chosen with their line settings, and opened in a
// tab, against `src/ipc/fake-serial.ts`.

import { expect, test } from "@playwright/test";

test("a serial port opens with the settings chosen", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".xterm-rows").first()).toContainText("$");
  await page.keyboard.press("Control+Shift+P");
  await page.getByPlaceholder("Type a command").fill("Open a serial port");
  await page.getByPlaceholder("Type a command").press("Enter");
  const dialog = page.getByRole("dialog", { name: "Open a serial port" });
  await expect(dialog.getByRole("list", { name: "Attached ports" })).toContainText(
    "USB: FTDI FT232R",
  );
  await expect(dialog.getByRole("combobox", { name: "Port" })).toHaveValue("/dev/ttyUSB0");
  await dialog.getByLabel("Speed (baud)").selectOption("9600");
  await dialog.getByLabel("Parity").selectOption("even");
  await dialog.getByLabel("Data bits").selectOption("7");
  await dialog.getByRole("button", { name: "Open" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("tab", { name: /\/dev\/ttyUSB0 9600 7E1/u })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(
    page.locator('section[data-pane-id][data-focused="true"] .xterm-rows'),
  ).toContainText("Opened /dev/ttyUSB0 at 9600 baud");
});
