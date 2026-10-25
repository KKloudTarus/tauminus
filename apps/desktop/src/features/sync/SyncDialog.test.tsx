// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";
import { render } from "vitest-browser-react";

import { fakeSync } from "@/ipc/fake-sync.ts";
import { sharedFakeVault } from "@/ipc/fake-vault.ts";
import { ConnectFailure } from "@/features/terminal/ConnectFailure.tsx";

import { SyncDialog, visible } from "./SyncDialog.tsx";
import { SyncStore } from "./sync-store.ts";

async function unlocked(): Promise<void> {
  const vault = sharedFakeVault();
  if (!(await vault.status()).exists) await vault.create("vault password");
  else await vault.unlock("vault password");
}

describe("SyncDialog", () => {
  it("joins, syncs, and lets a reviewed host through once accepted", async () => {
    await unlocked();
    const store = new SyncStore(fakeSync());
    let reviewed = 0;
    const screen = await render(
      <SyncDialog
        store={store}
        onClose={() => {}}
        onOpenVault={() => {}}
        onReviewed={(hosts) => (reviewed += hosts.length)}
      />,
    );
    await screen.getByRole("button", { name: "Choose…" }).click();
    await expect.element(screen.getByLabelText(/^Folder/)).toHaveValue("/home/me/Sync/tauminus");
    await screen.getByRole("radio", { name: /Joins one/ }).click();
    await screen.getByLabelText("Sync passphrase").fill("too short");
    await screen.getByRole("button", { name: "Join" }).click();
    await expect.element(screen.getByText(/at least 12 characters/)).toBeVisible();
    await screen.getByLabelText("Sync passphrase").fill("correct horse battery");
    await screen.getByRole("button", { name: "Join" }).click();
    await expect.element(screen.getByText("/home/me/Sync/tauminus")).toBeVisible();

    await screen.getByRole("button", { name: "Sync now" }).click();
    await expect.element(screen.getByText(/^Last synced/)).toBeVisible();
    await expect.element(screen.getByText(/the later change was kept/)).toBeVisible();

    const staging = screen.getByRole("article", { name: "staging" });
    await expect.element(staging.getByText('ProxyCommand("nc %h %p")')).toBeVisible();
    await expect.element(staging.getByText("cd /srv/app")).toBeVisible();
    await staging.getByRole("button", { name: "Accept staging" }).click();
    const deploy = screen.getByRole("article", { name: "deploy" });
    await expect.element(deploy.getByText(/identity, changed on another device/)).toBeVisible();
    await deploy.getByRole("button", { name: "Accept deploy" }).click();
    await expect
      .element(screen.getByRole("region", { name: "Changes to review" }))
      .not.toBeInTheDocument();
    expect(reviewed).toBe(2);
  });

  it("starts a sync only with matching passphrases, and stops it on request", async () => {
    await unlocked();
    const store = new SyncStore(fakeSync());
    const screen = await render(
      <SyncDialog store={store} onClose={() => {}} onOpenVault={() => {}} />,
    );
    await screen.getByRole("radio", { name: "A git remote" }).click();
    await screen.getByLabelText("Remote").fill("https://me:tok@example.com/s.git");
    await screen.getByLabelText("Sync passphrase").fill("a long sync passphrase");
    await screen.getByLabelText("Repeat it").fill("a long sync passphrasf");
    await screen.getByRole("button", { name: "Start syncing" }).click();
    await expect.element(screen.getByText(/do not match/)).toBeVisible();
    await screen.getByLabelText("Repeat it").fill("a long sync passphrase");
    await screen.getByRole("button", { name: "Start syncing" }).click();
    await expect.element(screen.getByText("https://example.com/s.git")).toBeVisible();
    await expect.element(screen.getByText(/tok@/)).not.toBeInTheDocument();
    await screen.getByRole("button", { name: /Stop syncing on this device/ }).click();
    await screen.getByRole("button", { name: "Stop syncing here" }).click();
    await expect.element(screen.getByRole("button", { name: "Start syncing" })).toBeVisible();
  });

  it("starts a sync on a bucket and shows it without its key", async () => {
    await unlocked();
    const screen = await render(
      <SyncDialog store={new SyncStore(fakeSync())} onClose={() => {}} onOpenVault={() => {}} />,
    );
    await screen.getByRole("radio", { name: "An S3 bucket" }).click();
    const start = screen.getByRole("button", { name: "Start syncing" });
    await expect.element(start).toBeDisabled();
    await screen.getByLabelText("Service address").fill("https://r2.example.com");
    await screen.getByLabelText("Region").fill("auto");
    await screen.getByLabelText("Bucket", { exact: true }).fill("team");
    await screen.getByLabelText("Access key id").fill("AKID");
    await screen.getByLabelText("Secret access key").fill("bucket-secret-1");
    await screen.getByLabelText("Sync passphrase").fill("a long sync passphrase");
    await screen.getByLabelText("Repeat it").fill("a long sync passphrase");
    await start.click();
    await expect.element(screen.getByText("team at https://r2.example.com")).toBeVisible();
    await expect.element(screen.getByText(/the S3 bucket/)).toBeVisible();
    expect(document.body.innerHTML).not.toContain("bucket-secret-1");
  });

  it("sends a locked vault to the vault", async () => {
    await unlocked();
    await sharedFakeVault().lock();
    let opened = 0;
    const screen = await render(
      <SyncDialog
        store={new SyncStore(fakeSync())}
        onClose={() => {}}
        onOpenVault={() => (opened += 1)}
      />,
    );
    await screen.getByRole("button", { name: "Open the vault" }).click();
    expect(opened).toBe(1);
  });
});

describe("visible", () => {
  it("shows what a screen would hide in startup text", () => {
    expect(visible("echo hi\u202e;rm -rf ~\r")).toBe("echo hi\\u{202e};rm -rf ~\n");
    expect(visible("a\u200bb")).toBe("a\\u{200b}b");
  });
});

describe("ConnectFailure", () => {
  it("offers a review for a host another device changed, then retries", async () => {
    let retried = 0;
    let asked = 0;
    const screen = await render(
      <ConnectFailure
        target="staging"
        code="needs_review"
        message="another device changed staging; review it before it is used"
        onRetry={() => (retried += 1)}
        onReview={(retry) => {
          asked += 1;
          retry();
        }}
      />,
    );
    await expect
      .element(screen.getByRole("heading"))
      .toHaveTextContent("Another device changed staging");
    await screen.getByRole("button", { name: "Review the change" }).click();
    expect([asked, retried]).toEqual([1, 1]);
  });
});
