// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Recording a pane and playing it back, against the in-memory recordings
// in `src/ipc/fake-recordings.ts`, sealed to the shared fake vault.

import { type Page, expect, test } from "@playwright/test";

const sidebar = (page: Page) => page.getByRole("complementary", { name: "Hosts" });
const vault = (page: Page) => page.getByRole("dialog", { name: "Vault" });
const recordings = (page: Page) => page.getByRole("dialog", { name: "Recordings" });
const screen = (page: Page) =>
  page.locator('section[data-pane-id][data-focused="true"] .xterm-rows');

async function palette(page: Page, command: string) {
  // The keys go to the pane: wait for it to have focus again.
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByPlaceholder("Type a command");
  await input.fill(command);
  await input.press("Enter");
}

async function run(page: Page, command: string) {
  await expect(page.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
  await page.keyboard.type(command);
  await expect(screen(page)).toContainText(command);
  await page.keyboard.press("Enter");
}

async function createVault(page: Page) {
  await sidebar(page).getByRole("button", { name: "Vault", exact: true }).click();
  await vault(page).getByLabel("Master password").fill("correct horse");
  await vault(page).getByLabel("Repeat it").fill("correct horse");
  await vault(page).getByRole("button", { name: "Create vault" }).click();
  await vault(page).getByRole("button", { name: "Close" }).click();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(screen(page)).toContainText("$");
});

test("records a pane from what it shows, then plays it back, searches, and bookmarks", async ({
  page,
}) => {
  test.setTimeout(90_000);
  await createVault(page);
  await run(page, "echo before recording");
  await palette(page, "Record this pane, or stop recording it");
  await expect(page.getByTestId("recording-status")).toBeVisible();
  await run(page, "echo during recording");
  await run(page, "echo the needle");
  await palette(page, "Record this pane, or stop recording it");
  await expect(page.getByTestId("recording-status")).toHaveCount(0);

  await palette(page, "Recordings: play back recorded sessions");
  const dialog = recordings(page);
  await dialog.getByRole("button", { name: /^Play / }).click();
  const player = dialog.getByTestId("player-screen");
  // It starts from the screen as it was when recording began.
  await expect(player).toContainText("before recording");
  await dialog.getByRole("button", { name: "Play" }).click();
  await expect(player).toContainText("the needle", { timeout: 10_000 });

  await dialog.getByLabel("Search the recording").fill("needle");
  const hit = dialog.getByRole("list", { name: "Search results" }).getByRole("button").first();
  await expect(hit).toContainText("the needle");
  const pause = dialog.getByRole("button", { name: "Pause" });
  if (await pause.isVisible()) await pause.click();
  await hit.click();
  await expect(player).toContainText("the needle");

  await dialog.getByLabel("Bookmark", { exact: true }).fill("found it");
  await dialog.getByLabel("Comment").fill("the line we wanted");
  await dialog.getByRole("button", { name: "Add" }).click();
  await expect(dialog.getByRole("list", { name: "Bookmarks" })).toContainText("found it");
  await expect(dialog.getByRole("list", { name: "Bookmarks" })).toContainText("the line we wanted");
});

test("a recording the backend stops loses its badge", async ({ page }) => {
  await createVault(page);
  await palette(page, "Record this pane, or stop recording it");
  await expect(page.getByTestId("recording-status")).toBeVisible();
  await page.evaluate(() => {
    const cut: unknown = Reflect.get(globalThis, "__tauminusCutRecordings");
    if (typeof cut === "function") cut();
  });
  await expect(page.getByTestId("recording-status")).toHaveCount(0, { timeout: 10_000 });
});

test("skip idle plays a long pause short", async ({ page }) => {
  await createVault(page);
  // Ten minutes of nothing between two lines.
  await page.evaluate(() => {
    const seed: unknown = Reflect.get(globalThis, "__tauminusSeedRecording");
    if (typeof seed !== "function") throw new Error("no test hooks");
    const cast = [
      '{"version":2,"width":80,"height":10}',
      JSON.stringify([0.1, "o", "first line\r\n"]),
      JSON.stringify([600.1, "o", "after the pause\r\n"]),
    ].join("\n");
    seed("a long pause", cast);
  });
  await palette(page, "Recordings: play back recorded sessions");
  const dialog = recordings(page);
  await dialog.getByRole("button", { name: "Play a long pause" }).click();
  const player = dialog.getByTestId("player-screen");
  await expect(dialog.getByTestId("player")).toBeVisible();
  await dialog.getByRole("button", { name: "Play" }).click();
  await expect(player).toContainText("first line");
  await expect(player).toContainText("after the pause", { timeout: 8000 });
});

test("an hour-long recording seeks anywhere in under 200 ms", async ({ page }) => {
  test.setTimeout(240_000);
  await createVault(page);
  // 40 MB of colourful output over an hour, with full-screen redraws.
  const id = await page.evaluate(async () => {
    const make: unknown = Reflect.get(globalThis, "__tauminusHourLongCast");
    const seed: unknown = Reflect.get(globalThis, "__tauminusSeedRecording");
    if (typeof make !== "function" || typeof seed !== "function") throw new Error("no test hooks");
    const cast: unknown = await make(40 * 1024 * 1024);
    return seed("an hour of logs", cast);
  });
  expect(id).toBeTruthy();
  await palette(page, "Recordings: play back recorded sessions");
  const dialog = recordings(page);
  await dialog.getByRole("button", { name: "Play an hour of logs" }).click();
  await expect(dialog.getByTestId("player")).toBeVisible({ timeout: 60_000 });
  // Seeking is prepared in the background; then every seek is fast.
  await expect(dialog.getByTestId("index-progress")).toHaveCount(0, { timeout: 180_000 });
  const position = dialog.getByLabel("Position");
  const times: number[] = [];
  for (const share of [0.9, 0.1, 0.55, 0.02, 0.99, 0.33, 0.7, 0.45]) {
    // oxlint-disable-next-line no-await-in-loop -- one seek after the other
    const before = Number(await position.getAttribute("data-seeks"));
    // oxlint-disable-next-line no-await-in-loop -- one seek after the other
    await position.evaluate((input, at) => {
      if (!(input instanceof HTMLInputElement)) throw new Error("not an input");
      const seconds = Number(input.max) * at;
      // React reads the value through the prototype's setter.
      const setter: unknown = Reflect.get(
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value") ?? {},
        "set",
      );
      if (typeof setter === "function") Reflect.apply(setter, input, [String(seconds)]);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, share);
    // oxlint-disable-next-line no-await-in-loop -- waits for this seek
    await expect(position).toHaveAttribute("data-seeks", String(before + 1), { timeout: 5000 });
    // oxlint-disable-next-line no-await-in-loop -- read in turn
    times.push(Number(await position.getAttribute("data-last-seek-ms")));
  }
  test.info().annotations.push({ type: "seek ms", description: times.join(", ") });
  expect(Math.max(...times)).toBeLessThan(200);
});

test("shows the recording badge beside the broadcast one", async ({ page }) => {
  await createVault(page);
  await palette(page, "Split pane right");
  const pane = page.locator('section[data-pane-id][data-focused="true"]');
  // Two panes now: run commands once the focused one has the keyboard.
  const command = async (name: string) => {
    await expect(pane.getByRole("textbox", { name: "Terminal input" })).toBeFocused();
    await page.keyboard.press("Control+Shift+P");
    await page.getByPlaceholder("Type a command").fill(name);
    await page.keyboard.press("Enter");
  };
  await command("Broadcast input to every pane in this tab");
  await command("Record this pane, or stop recording it");
  const rec = await pane.getByTestId("recording-status").boundingBox();
  const broadcast = await pane.getByTestId("broadcast-badge").boundingBox();
  if (!rec || !broadcast) throw new Error("a badge is missing");
  // toBeVisible ignores what covers an element; the boxes must not overlap.
  const apart =
    rec.y >= broadcast.y + broadcast.height ||
    broadcast.y >= rec.y + rec.height ||
    rec.x >= broadcast.x + broadcast.width ||
    broadcast.x >= rec.x + rec.width;
  expect(apart).toBe(true);
});

test("sizes the recordings dialog to its content while it has none", async ({ page }) => {
  await palette(page, "Recordings: play back recorded sessions");
  await expect(recordings(page)).toContainText("No recordings yet");
  const box = await recordings(page).boundingBox();
  expect(box?.height ?? 0).toBeLessThan(200);
});
