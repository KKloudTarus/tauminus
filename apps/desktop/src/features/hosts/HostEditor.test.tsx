// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { fakeHosts } from "@/ipc/fake-hosts.ts";

import { EditorDialog, withEdits } from "./HostEditor.tsx";
import { HostsStore } from "./hosts-store.ts";

describe("withEdits", () => {
  it("keeps what the form does not show", () => {
    const base = {
      route: { kind: "proxyCommand", command: "nc %h %p" } as const,
      methods: ["agent", "password"] as ("agent" | "password")[],
      identitiesOnly: true,
    };
    const edited = withEdits(base, { user: "deploy", port: "2222", jumps: "" });
    expect(edited).toMatchObject({ ...base, user: "deploy", port: 2222, jumps: null });
  });

  it("maps blank fields to inherit and the direct box to no jump hosts", () => {
    expect(withEdits({ user: "old" }, { user: " ", port: "", jumps: "a, b" })).toMatchObject({
      user: null,
      port: null,
      jumps: ["a", "b"],
    });
    expect(withEdits({}, { user: "", port: "", jumps: "a", direct: true }).jumps).toEqual([]);
  });
});

describe("EditorDialog", () => {
  it("saves an edit without dropping the route or methods", async () => {
    const client = fakeHosts();
    await client.saveHost({
      label: "legacy",
      address: "legacy.internal",
      settings: { route: { kind: "proxyCommand", command: "nc %h %p" }, methods: ["agent"] },
    });
    const store = new HostsStore(client);
    await store.load();
    const host = store.getSnapshot().view.hosts[0]!;
    let closed = false;
    const screen = await render(
      <EditorDialog
        store={store}
        editing={{ kind: "host", host }}
        onClose={() => (closed = true)}
      />,
    );
    await expect.element(screen.getByText(/Connects through ProxyCommand nc %h %p/)).toBeVisible();
    await screen.getByLabelText("Port").fill("2200");
    await screen.getByRole("button", { name: "Save" }).click();
    await expect.poll(() => closed).toBe(true);
    const saved = store.getSnapshot().view.hosts[0]!;
    expect(saved.settings).toMatchObject({
      port: 2200,
      route: { kind: "proxyCommand", command: "nc %h %p" },
      methods: ["agent"],
    });
  });
});
