// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { UpdateBanner } from "./UpdateBanner.tsx";
import { UpdatesStore } from "./updates-store.ts";

describe("UpdateBanner", () => {
  it("offers a version with its notes, installs on request, and hides on Later", async () => {
    let installed = 0;
    const store = new UpdatesStore(
      {
        check: async () => ({
          version: "0.6.0",
          current: "0.5.0",
          date: null,
          notes: "Faster sync.\n<b>not markup</b>",
        }),
        install: async () => {
          installed += 1;
        },
      },
      undefined,
    );
    const screen = await render(<UpdateBanner store={store} />);
    await expect.element(screen.getByText(/0\.6\.0 is available/)).not.toBeInTheDocument();
    await store.check(false);
    await expect
      .element(screen.getByText("Tauminus 0.6.0 is available; this is 0.5.0."))
      .toBeVisible();
    await screen.getByText("What changed").click();
    await expect.element(screen.getByText(/<b>not markup<\/b>/)).toBeVisible();
    await screen.getByRole("button", { name: "Install and restart" }).click();
    await expect.poll(() => installed).toBe(1);
    await expect.element(screen.getByText(/Installing Tauminus 0\.6\.0/)).toBeVisible();
  });

  it("answers a check the user asked for, and Dismiss hides it", async () => {
    const store = new UpdatesStore({ check: async () => null, install: async () => {} }, undefined);
    await store.check();
    const screen = await render(<UpdateBanner store={store} />);
    await expect.element(screen.getByText("This is the newest version of Tauminus.")).toBeVisible();
    await expect
      .element(screen.getByRole("button", { name: "Install and restart" }))
      .not.toBeInTheDocument();
    await screen.getByRole("button", { name: "Dismiss" }).click();
    await expect
      .element(screen.getByText("This is the newest version of Tauminus."))
      .not.toBeInTheDocument();
  });
});
