// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { userEvent } from "vitest/browser";
import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { IMPORT_ENTRIES, ImportMenu } from "./ImportMenu.tsx";

describe("ImportMenu", () => {
  it("lists every source, moves with the arrows, and closes on Escape", async () => {
    const picked: string[] = [];
    const screen = await render(<ImportMenu onImport={(kind) => picked.push(kind)} />);
    const button = screen.getByRole("button", { name: "Import hosts" });
    await button.click();
    const items = screen.getByRole("menuitem");
    await expect.poll(() => items.all().length).toBe(IMPORT_ENTRIES.length);
    await expect.element(items.first()).toHaveFocus();
    await userEvent.keyboard("{ArrowUp}");
    await expect.element(items.last()).toHaveFocus();
    await userEvent.keyboard("{Escape}");
    await expect.element(screen.getByRole("menu")).not.toBeInTheDocument();
    await expect.element(button).toHaveFocus();

    await button.click();
    await screen.getByRole("menuitem", { name: /SecureCRT/ }).click();
    expect(picked).toEqual(["secureCrt"]);
    await expect.element(screen.getByRole("menu")).not.toBeInTheDocument();
  });
});
