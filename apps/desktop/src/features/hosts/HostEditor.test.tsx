// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { fakeHosts } from "@/ipc/fake-hosts.ts";

import {
  EditorDialog,
  guardFields,
  sessionFields,
  withEdits,
  withGuard,
  withSession,
} from "./HostEditor.tsx";
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

describe("withGuard", () => {
  it("round-trips the guard settings and drops an empty one", () => {
    const guard = { enabled: false, allow: ["ls *"], deny: ["reboot", "rm -rf /x, y"] };
    expect(withGuard({ user: "u" }, guardFields(guard))).toEqual({ user: "u", guard });
    expect(withGuard({ guard }, guardFields(null))).toEqual({ guard: null });
    expect(withGuard({}, { enabled: "on", allow: "  \n a \n\n", deny: "" }).guard).toEqual({
      enabled: true,
      allow: ["a"],
      deny: [],
    });
  });
});

describe("withSession", () => {
  it("round-trips agent forwarding and the environment", () => {
    const settings = {
      forwardAgent: true,
      env: [
        { name: "LANG", value: "C.UTF-8" },
        { name: "MOTD", value: "a=b c" },
      ],
    };
    expect(withSession({ user: "u" }, sessionFields(settings))).toEqual({ user: "u", ...settings });
    expect(withSession(settings, sessionFields({}))).toEqual({ forwardAgent: null, env: null });
  });

  it("keeps spaces at the end of a value and takes CRLF lines", () => {
    const result = withSession({}, { forwardAgent: "inherit", env: "X=a \r\nY=b=c\r\n" });
    expect(result).toMatchObject({
      env: [
        { name: "X", value: "a " },
        { name: "Y", value: "b=c" },
      ],
    });
  });

  it("reports a line that is not NAME=value", () => {
    expect(withSession({}, { forwardAgent: "inherit", env: "LANG" })).toBe(
      '"LANG" is not NAME=value',
    );
    expect(withSession({}, { forwardAgent: "inherit", env: "=x" })).toMatch(/not NAME=value/);
  });
});

describe("EditorDialog", () => {
  it("turns the prod guard on with a deny pattern", async () => {
    const client = fakeHosts();
    await client.saveHost({ label: "db", address: "db.internal" });
    const store = new HostsStore(client);
    await store.load();
    const host = store.getSnapshot().view.hosts[0]!;
    expect(host.guarded).toBe(false);
    let closed = false;
    const screen = await render(
      <EditorDialog
        store={store}
        editing={{ kind: "host", host }}
        onClose={() => (closed = true)}
      />,
    );
    await screen.getByText("Prod guard").click();
    await screen
      .getByRole("combobox", { name: "Check commands before they run" })
      .selectOptions("on");
    await screen.getByLabelText(/Always ask before/).fill("systemctl stop *");
    await screen.getByRole("button", { name: "Save" }).click();
    await expect.poll(() => closed).toBe(true);
    const saved = store.getSnapshot().view.hosts[0]!;
    expect(saved.guarded).toBe(true);
    expect(saved.settings.guard).toEqual({ enabled: true, allow: [], deny: ["systemctl stop *"] });
  });

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
