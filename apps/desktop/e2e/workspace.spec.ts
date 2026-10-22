// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type Locator, type Page, expect, test } from "@playwright/test";

const panes = (page: Page) => page.locator("section[data-pane-id]");
const focusedPane = (page: Page) => page.locator('section[data-pane-id][data-focused="true"]');
const tabs = (page: Page) => page.getByRole("tab");
const screenOf = (pane: Locator) => pane.locator(".xterm-rows");

async function typeIn(page: Page, text: string) {
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screenOf(focusedPane(page))).toContainText("$");
  await focusedPane(page).click();
});

test("runs commands in the first terminal", async ({ page }) => {
  await typeIn(page, "echo hello from e2e");
  await expect(screenOf(focusedPane(page))).toContainText("hello from e2e");
});

test("opens, cycles and closes tabs with shortcuts", async ({ page }) => {
  await page.keyboard.press("Control+Shift+T");
  await expect(tabs(page)).toHaveCount(2);
  await expect(tabs(page).nth(1)).toHaveAttribute("aria-selected", "true");

  await page.keyboard.press("Control+Tab");
  await expect(tabs(page).nth(0)).toHaveAttribute("aria-selected", "true");

  await page.keyboard.press("Control+Shift+W");
  await expect(tabs(page)).toHaveCount(1);
});

test("keeps each tab's screen when switching tabs", async ({ page }) => {
  await typeIn(page, "echo first-tab");
  await page.keyboard.press("Control+Shift+T");
  await expect(screenOf(focusedPane(page))).not.toContainText("first-tab");
  await page.keyboard.press("Control+Shift+Tab");
  await expect(screenOf(focusedPane(page))).toContainText("first-tab");
});

test("splits panes and moves focus between them", async ({ page }) => {
  await page.keyboard.press("Control+Shift+D");
  await expect(panes(page)).toHaveCount(2);
  const right = await focusedPane(page).getAttribute("data-pane-id");
  await typeIn(page, "echo right-pane");

  await page.keyboard.press("Alt+ArrowLeft");
  await expect(focusedPane(page)).not.toHaveAttribute("data-pane-id", right ?? "");
  await typeIn(page, "echo left-pane");

  await expect(page.locator(`section[data-pane-id="${right}"] .xterm-rows`)).toContainText(
    "right-pane",
  );
  await expect(page.locator(`section[data-pane-id="${right}"] .xterm-rows`)).not.toContainText(
    "left-pane",
  );

  await page.keyboard.press("Control+Shift+E");
  await expect(panes(page)).toHaveCount(3);
});

test("closes a pane whose shell exits cleanly, and keeps one that fails", async ({ page }) => {
  await page.keyboard.press("Control+Shift+D");
  await expect(panes(page)).toHaveCount(2);
  await typeIn(page, "exit 0");
  await expect(panes(page)).toHaveCount(1);

  await typeIn(page, "exit 3");
  await expect(panes(page)).toHaveCount(1);
  await expect(page.getByRole("status")).toContainText("Process exited with code 3");
});

test("an ended pane stays ended when a neighbour closes", async ({ page }) => {
  await typeIn(page, "exit 3");
  await expect(page.getByRole("status")).toContainText("Process exited with code 3");
  const ended = await focusedPane(page).getAttribute("data-pane-id");
  await page.keyboard.press("Control+Shift+D");
  await expect(panes(page)).toHaveCount(2);
  await page.keyboard.press("Control+Shift+X");
  await expect(panes(page)).toHaveCount(1);
  // The remount must not start a new shell under the old screen.
  const screen = page.locator(`section[data-pane-id="${ended}"] .xterm-rows`);
  await expect(page.getByRole("status")).toContainText("Process exited with code 3");
  await expect(screen).toHaveText(/^\$ exit 3\s*$/);
});

test("shows the title a program sets", async ({ page }) => {
  await typeIn(page, "title deploy@prod-db-2");
  await expect(tabs(page).first()).toContainText("deploy@prod-db-2");
});

test("runs commands from the palette", async ({ page }) => {
  await page.keyboard.press("Control+Shift+P");
  const palette = page.getByRole("dialog");
  await expect(palette).toBeVisible();
  await page.keyboard.type("split pane right");
  await page.keyboard.press("Enter");
  await expect(palette).toBeHidden();
  await expect(panes(page)).toHaveCount(2);
});

test("switches theme from the palette and remembers it", async ({ page }) => {
  const html = page.locator("html");
  await page.keyboard.press("Control+Shift+P");
  await page.keyboard.type("switch theme");
  await page.keyboard.press("Enter");
  await expect(html).toHaveClass(/light/);
  await page.reload();
  await expect(html).toHaveClass(/light/);
});

test("reorders tabs by dragging", async ({ page }) => {
  await typeIn(page, "title alpha");
  await page.keyboard.press("Control+Shift+T");
  await focusedPane(page).click();
  await typeIn(page, "title beta");
  await expect(tabs(page)).toHaveText([/alpha/, /beta/]);

  await tabs(page).nth(1).dragTo(tabs(page).nth(0));
  await expect(tabs(page)).toHaveText([/beta/, /alpha/]);
});

test("leaves plain Ctrl+K to the shell", async ({ page }) => {
  await page.keyboard.press("Control+K");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("opens a link only on Ctrl+click, and only http and https", async ({ page }) => {
  await typeIn(page, "echo docs https://example.com/docs file:///etc/passwd");
  const screen = screenOf(focusedPane(page));
  await expect(screen).toContainText("https://example.com/docs");
  const terminal = focusedPane(page).locator(".xterm");
  const opened = () => page.evaluate(() => window.tauminusOpenedLinks ?? []);
  // A point inside `text` on screen. xterm.js may split a word across spans
  // (a hovered link, a selection), so this walks the spans of the row.
  const pointAt = async (text: string) => {
    const row = screen.locator("> div", { hasText: text }).last();
    const spans = await row.locator("span").evaluateAll((nodes) =>
      nodes.map((node) => {
        const box = node.getBoundingClientRect();
        return {
          text: node.textContent ?? "",
          x: box.x,
          y: box.y,
          width: box.width,
          height: box.height,
        };
      }),
    );
    const line = spans.map((span) => span.text).join("");
    const at = line.indexOf(text);
    if (at < 0) throw new Error(`no ${text} on screen`);
    let start = 0;
    for (const span of spans) {
      if (at < start + span.text.length) {
        const cell = span.width / span.text.length;
        return { x: span.x + (at - start + 1.5) * cell, y: span.y + span.height / 2 };
      }
      start += span.text.length;
    }
    throw new Error(`no ${text} on screen`);
  };
  const { x, y } = await pointAt("example.com");

  // xterm finds links on hover; its title says how to open one.
  await page.mouse.move(x, y + 40);
  await page.mouse.move(x, y);
  await expect(terminal).toHaveAttribute("title", "Ctrl+click to open");

  // A plain click selects; it never opens a page.
  await page.mouse.click(x, y);
  expect(await opened()).toEqual([]);

  await page.mouse.move(x, y + 40);
  await page.mouse.move(x + 2, y);
  await expect(terminal).toHaveAttribute("title", "Ctrl+click to open");
  await page.keyboard.down("Control");
  await page.mouse.click(x + 2, y);
  await page.keyboard.up("Control");
  await expect.poll(opened).toEqual(["https://example.com/docs"]);

  // file: is not a link Tauminus offers to open.
  const passwd = await pointAt("passwd");
  await page.mouse.move(passwd.x, passwd.y);
  await page.keyboard.down("Control");
  await page.mouse.click(passwd.x, passwd.y);
  await page.keyboard.up("Control");
  expect(await opened()).toEqual(["https://example.com/docs"]);
});

test("takes text from an input method, as Telex sends it", async ({ page }) => {
  // An IME such as ibus-unikey composes "tiếng" from "tieesng": the page
  // sees a composition, then the committed text once.
  await page.keyboard.type("echo ");
  const cdp = await page.context().newCDPSession(page);
  for (const step of ["t", "ti", "tie", "tiê", "tiế", "tiến", "tiếng"]) {
    // oxlint-disable-next-line no-await-in-loop -- composition steps arrive in order
    await cdp.send("Input.imeSetComposition", {
      text: step,
      selectionStart: step.length,
      selectionEnd: step.length,
    });
  }
  await cdp.send("Input.insertText", { text: "tiếng" });
  // A person types the next key after the committed word is on screen;
  // xterm.js settles a composition on a timer that must run first.
  const prompt = screenOf(focusedPane(page)).locator("> div").first();
  await expect(prompt).toContainText("echo tiếng");
  await page.keyboard.type(" ");
  for (const step of ["V", "Vi", "Việ", "Việt"]) {
    // oxlint-disable-next-line no-await-in-loop -- composition steps arrive in order
    await cdp.send("Input.imeSetComposition", {
      text: step,
      selectionStart: step.length,
      selectionEnd: step.length,
    });
  }
  await cdp.send("Input.insertText", { text: "Việt" });
  await expect(prompt).toContainText("echo tiếng Việt");
  await page.keyboard.press("Enter");
  const rows = screenOf(focusedPane(page)).locator("> div");
  // The echo prints the text once: nothing doubled, nothing lost.
  await expect(rows.nth(1)).toHaveText(/^tiếng Việt\s*$/);
});

test("takes text from IBus on WebKitGTK, which never starts a composition", async ({ page }) => {
  await page.keyboard.type("echo ");
  // What WebKitGTK sends for ibus-unikey typing "tieesng Vieejt", all
  // inside the keydown: keydown 229, beforeinput and input of type
  // insertFromComposition, compositionend. No compositionstart.
  for (const word of ["tiếng ", "Việt"]) {
    // oxlint-disable-next-line no-await-in-loop -- one commit after the other, as typed
    await page.evaluate((text) => {
      const textarea = document.querySelector<HTMLTextAreaElement>(
        'section[data-focused="true"] textarea',
      );
      if (!textarea) throw new Error("no terminal textarea");
      // Chromium blanks input types it does not implement; WebKit sends this one.
      const composed = (type: string) => {
        const event = new InputEvent(type, { data: text, bubbles: true });
        Object.defineProperty(event, "inputType", { value: "insertFromComposition" });
        return event;
      };
      const keydown = new KeyboardEvent("keydown", { key: "Unidentified", bubbles: true });
      Object.defineProperty(keydown, "keyCode", { value: 229 });
      textarea.dispatchEvent(keydown);
      textarea.dispatchEvent(composed("beforeinput"));
      textarea.value += text;
      textarea.dispatchEvent(composed("input"));
      textarea.dispatchEvent(new CompositionEvent("compositionend", { data: text, bubbles: true }));
    }, word);
  }
  await page.keyboard.press("Enter");
  const rows = screenOf(focusedPane(page)).locator("> div");
  await expect(rows.nth(1)).toHaveText(/^tiếng Việt\s*$/);
});

test("keeps spaces in the input textarea, so WebKit never commits them as U+00A0", async ({
  page,
}) => {
  const whiteSpace = await page
    .locator('section[data-focused="true"] textarea')
    .evaluate((textarea) => getComputedStyle(textarea).whiteSpace);
  expect(whiteSpace).toBe("pre");
});
