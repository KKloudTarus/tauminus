// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Layout across window sizes: the phone drawer, dialogs that fit the window,
// touch target sizes, and the theme switch.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const hostsButton = (page: Page) => page.getByRole("button", { name: "Hosts", exact: true });
const focusedScreen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

const PHONE = { width: 390, height: 844 };

async function noSideScroll(page: Page) {
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
}

async function importConfig(page: Page) {
  await sidebar(page).getByRole("button", { name: "Import 3 hosts from ~/.ssh/config" }).click();
  await expect(sidebar(page).getByRole("status")).toHaveText(/Imported 3 hosts/);
}

test.describe("on a phone", () => {
  test.use({ viewport: PHONE });

  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    await expect(focusedScreen(page)).toContainText("$");
  });

  test("starts on the terminal and opens the hosts as a drawer", async ({ page }) => {
    await expect(sidebar(page)).toHaveCount(0);
    await expect(hostsButton(page)).toHaveAttribute("aria-pressed", "false");
    await noSideScroll(page);

    await hostsButton(page).click();
    await expect(sidebar(page)).toBeVisible();
    const box = await sidebar(page).boundingBox();
    // The drawer leaves a strip of the terminal to tap away.
    expect(box?.width).toBeLessThanOrEqual(PHONE.width - 48);
    await noSideScroll(page);

    await page.getByRole("button", { name: "Close hosts" }).click();
    await expect(sidebar(page)).toHaveCount(0);
  });

  test("keeps keys out of the terminal the drawer covers", async ({ page }) => {
    await hostsButton(page).click();
    const search = sidebar(page).getByRole("searchbox", { name: "Search hosts" });
    await expect(search).toBeFocused();
    await page.keyboard.type("zz");
    await expect(search).toHaveValue("zz");
    await expect(focusedScreen(page)).not.toContainText("zz");

    await page.keyboard.press("Escape"); // clears the search
    await page.keyboard.press("Escape"); // closes the drawer
    await expect(sidebar(page)).toHaveCount(0);
    await page.keyboard.type("echo back");
    await expect(focusedScreen(page)).toContainText("echo back");
  });

  test("closes the drawer on connect, and Escape in its dialogs leaves it open", async ({
    page,
  }) => {
    await hostsButton(page).click();
    await importConfig(page);

    await sidebar(page).getByRole("button", { name: "New host" }).click();
    const editor = page.getByRole("dialog", { name: "New host" });
    await expect(editor).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(editor).toHaveCount(0);
    await expect(sidebar(page)).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(sidebar(page)).toHaveCount(0);

    await hostsButton(page).click();
    await sidebar(page)
      .getByRole("button", { name: /^prompt alice@prompt\.test/ })
      .click();
    await expect(sidebar(page)).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Trust once" })).toBeVisible();
  });

  test("shows dialogs as sheets along the bottom edge", async ({ page }) => {
    await hostsButton(page).click();
    await sidebar(page).getByRole("button", { name: "New host" }).click();
    const editor = page.getByRole("dialog", { name: "New host" });
    // Measure the resting sheet, not its opening animation.
    await page.waitForFunction(() => document.getAnimations().length === 0);
    const box = await editor.boundingBox();
    expect(box?.width).toBe(PHONE.width);
    expect(Math.round((box?.y ?? 0) + (box?.height ?? 0))).toBe(PHONE.height);
    await expect(editor.getByRole("button", { name: "Save" })).toBeInViewport();
  });
});

test("hides the docked hosts in a narrow window and brings them back", async ({ page }) => {
  await page.goto("/");
  await expect(sidebar(page)).toBeVisible();
  await page.setViewportSize(PHONE);
  await expect(sidebar(page)).toHaveCount(0);
  await page.setViewportSize({ width: 1200, height: 760 });
  await expect(sidebar(page)).toBeVisible();
});

test("keeps the saved host-list choice through a narrow window", async ({ page }) => {
  await page.goto("/");
  await expect(sidebar(page)).toBeVisible();
  await page.setViewportSize(PHONE);
  await expect(sidebar(page)).toHaveCount(0);
  await hostsButton(page).click();
  await importConfig(page);
  await sidebar(page)
    .getByRole("button", { name: /^prompt alice@prompt\.test/ })
    .click();
  await expect(sidebar(page)).toHaveCount(0);
  // Quit while narrow, start again wide: the list is open, as last chosen there.
  await page.reload();
  await page.setViewportSize({ width: 1200, height: 760 });
  await expect(sidebar(page)).toBeVisible();
});

test("names groups as written, whatever their typeface", async ({ page }) => {
  await page.goto("/");
  await importConfig(page);
  // The engine's own accessibility tree, which applies text-transform; the
  // role queries in other tests read the DOM and would not notice.
  const cdp = await page.context().newCDPSession(page);
  const { nodes } = (await cdp.send("Accessibility.getFullAXTree")) as {
    nodes: { role?: { value?: string }; name?: { value?: string } }[];
  };
  const buttons = nodes.filter((node) => node.role?.value === "button");
  const names = buttons.map((node) => node.name?.value);
  expect(names).toContain("SSH config");
  expect(names).not.toContain("SSH CONFIG");
});

test("marks the focused pane only once a tab is split", async ({ page }) => {
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
  const ring = () =>
    page
      .locator('section[data-pane-id][data-focused="true"]')
      .evaluate((element) => getComputedStyle(element).boxShadow);
  expect(await ring()).toBe("none");
  await page.locator("body").press("Control+Shift+D");
  await expect(page.locator("section[data-pane-id]")).toHaveCount(2);
  await expect.poll(ring).not.toBe("none");
});

test.describe("on a touch screen", () => {
  test.use({ viewport: { width: 820, height: 1180 }, hasTouch: true, isMobile: true });

  test("gives buttons and fields 40 px targets", async ({ page }) => {
    await page.goto("/");
    await expect(focusedScreen(page)).toContainText("$");
    await importConfig(page);
    const names = [
      "Hosts",
      "New tab",
      "All commands",
      "Edit prompt",
      "Files on prompt",
      "Edit group SSH config",
      "Dismiss",
    ];
    const boxes = await Promise.all(
      names.map((name) => page.getByRole("button", { name, exact: true }).boundingBox()),
    );
    for (const [index, box] of boxes.entries()) {
      expect(box?.height, names[index]).toBeGreaterThanOrEqual(40);
      expect(box?.width, names[index]).toBeGreaterThanOrEqual(40);
    }
  });
});

test("switches the theme from the tab bar, terminal included", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("tauminus.theme", "system"));
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
  const terminalBackground = () =>
    page.evaluate(() => {
      // xterm.js 6 paints its theme background here.
      const viewport = document.querySelector(
        'section[data-focused="true"] .xterm-scrollable-element',
      );
      return viewport ? getComputedStyle(viewport).backgroundColor : "";
    });
  await expect.poll(terminalBackground).toBe("rgb(14, 15, 19)");

  await page.getByRole("button", { name: "Theme: system" }).click();
  await expect(page.locator("html")).toHaveClass(/\blight\b/u);
  await expect(page.getByRole("button", { name: "Theme: light" })).toBeVisible();
  await expect.poll(terminalBackground).toBe("rgb(255, 255, 255)");

  await page.getByRole("button", { name: "Theme: light" }).click();
  await expect(page.locator("html")).toHaveClass(/\bdark\b/u);
  await expect.poll(terminalBackground).toBe("rgb(14, 15, 19)");
});

test("shows code as typed, without ligatures", async ({ page }) => {
  await page.goto("/");
  await sidebar(page).getByRole("button", { name: "Import 3 hosts from ~/.ssh/config" }).click();
  await sidebar(page)
    .getByRole("button", { name: /^prompt alice@prompt\.test/ })
    .click();
  const fingerprint = page.getByTestId("host-key-fingerprint");
  await expect(fingerprint).toBeVisible();
  const ligatures = (locator: typeof fingerprint) =>
    locator.evaluate((element) => getComputedStyle(element).fontVariantLigatures);
  expect(await ligatures(fingerprint)).toBe("none");
  expect(await ligatures(page.locator(".xterm").first())).toBe("none");
});

test("follows the system theme when the OS switches", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("tauminus.theme", "system"));
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await expect(focusedScreen(page)).toContainText("$");
  const terminalBackground = () =>
    page.evaluate(() => {
      const viewport = document.querySelector(
        'section[data-focused="true"] .xterm-scrollable-element',
      );
      return viewport ? getComputedStyle(viewport).backgroundColor : "";
    });
  await expect.poll(terminalBackground).toBe("rgb(14, 15, 19)");
  await page.emulateMedia({ colorScheme: "light" });
  await expect.poll(terminalBackground).toBe("rgb(255, 255, 255)");
});
