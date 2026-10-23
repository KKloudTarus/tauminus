// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { SnippetForm } from "./SnippetForm.tsx";

describe("SnippetForm", () => {
  it("starts at the defaults and refuses a blank value that has none", async () => {
    const runs: Record<string, string>[] = [];
    const screen = await render(
      <SnippetForm
        snippet={{
          id: "s",
          name: "clean",
          folder: "Ops",
          body: "rm -rf /srv/{{site}} --keep {{keep:7}}",
          variables: [
            { name: "site", default: null },
            { name: "keep", default: "7" },
          ],
        }}
        onRun={(values) => runs.push(values)}
        onClose={() => {}}
      />,
    );
    await expect.element(screen.getByLabelText("keep")).toHaveValue("7");
    await screen.getByRole("button", { name: "Run" }).click();
    expect(runs).toEqual([]);
    await screen.getByLabelText("site").fill("blog");
    await screen.getByRole("button", { name: "Run" }).click();
    expect(runs).toEqual([{ site: "blog", keep: "7" }]);
  });
});
