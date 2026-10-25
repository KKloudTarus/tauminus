// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { fakeHosts } from "@/ipc/fake-hosts.ts";

import { CloudImportDialog } from "./CloudImportDialog.tsx";
import { HostsStore } from "./hosts-store.ts";

describe("CloudImportDialog", () => {
  it("shows what an import would do, saves nothing until accepted, then imports it", async () => {
    const store = new HostsStore(fakeHosts());
    await store.load();
    let closed = false;
    const screen = await render(
      <CloudImportDialog store={store} onClose={() => (closed = true)} />,
    );
    await screen.getByRole("combobox", { name: "Provider" }).selectOptions("digitalOcean");
    await expect.element(screen.getByText(/Context \(blank/)).toBeVisible();
    await screen.getByRole("button", { name: "List" }).click();
    await expect.element(screen.getByText("Ran /usr/local/bin/doctl for b6fc48a1.")).toBeVisible();
    await expect.element(screen.getByText("web-1 203.0.113.7 (nyc3)")).toBeVisible();
    await expect
      .element(screen.getByText(/meta: 169\.254\.169\.254 is not an address/))
      .toBeVisible();
    expect(store.getSnapshot().view.hosts).toHaveLength(0);
    await screen.getByRole("button", { name: "Import" }).click();
    await expect.poll(() => closed).toBe(true);
    expect(
      store
        .getSnapshot()
        .view.hosts.map((h) => h.label)
        .toSorted(),
    ).toEqual(["db-1", "web-1"]);
    expect(store.getSnapshot().notice).toContain("Imported 2 hosts from DigitalOcean");
  });

  it("says what the CLI said when it fails", async () => {
    const store = new HostsStore(fakeHosts());
    await store.load();
    const screen = await render(<CloudImportDialog store={store} onClose={() => {}} />);
    await screen.getByRole("textbox").fill("broken");
    await screen.getByRole("button", { name: "List" }).click();
    await expect.element(screen.getByText(/Unable to authenticate you/)).toBeVisible();
    await expect.element(screen.getByRole("button", { name: "Import" })).not.toBeInTheDocument();
  });
});
