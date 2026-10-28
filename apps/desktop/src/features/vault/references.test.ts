// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import {
  addsReferences,
  blankReference,
  hasReferences,
  managersFor,
  sameReferences,
  setReferences,
  vaultBeside,
} from "./references.ts";

const op = { manager: "onePassword", reference: "op://Ops/db/password" } as const;
const kv = {
  manager: "vault",
  address: "https://vault.example.com",
  mount: "secret",
  path: "ssh/deploy",
  field: "key",
} as const;

describe("password manager references", () => {
  it("offers Bitwarden for a password and a passphrase, never for a key", () => {
    expect(managersFor("password").map((m) => m.value)).toContain("bitwarden");
    expect(managersFor("passphrase").map((m) => m.value)).toContain("bitwarden");
    expect(managersFor("privateKey").map((m) => m.value)).toEqual(["onePassword", "vault"]);
  });

  it("starts each manager blank, and Vault on the usual KV mount", () => {
    expect(blankReference("onePassword")).toEqual({ manager: "onePassword", reference: "" });
    expect(blankReference("bitwarden")).toEqual({ manager: "bitwarden", item: "" });
    expect(blankReference("vault")).toEqual({
      manager: "vault",
      address: "",
      mount: "secret",
      path: "",
      field: "",
    });
  });

  it("drops the slots that hold nothing", () => {
    expect(setReferences({ password: op, privateKey: null })).toEqual({
      password: op,
    });
    expect(hasReferences({ password: null })).toBe(false);
    expect(hasReferences({ passphrase: op })).toBe(true);
  });

  it("tells an unchanged set from a changed one, field by field", () => {
    expect(sameReferences({ password: op }, { password: { ...op }, privateKey: null })).toBe(true);
    expect(sameReferences({ privateKey: kv }, { privateKey: { ...kv } })).toBe(true);
    for (const field of ["address", "mount", "path", "field"] as const) {
      expect(sameReferences({ privateKey: kv }, { privateKey: { ...kv, [field]: "x" } })).toBe(
        false,
      );
    }
    expect(sameReferences({ password: op }, { password: { ...op, reference: "op://a/b/c" } })).toBe(
      false,
    );
    expect(sameReferences({ password: op }, { passphrase: op })).toBe(false);
    expect(
      sameReferences(
        { password: { manager: "bitwarden", item: "a" } },
        { password: { manager: "bitwarden", item: "b" } },
      ),
    ).toBe(false);
    expect(
      sameReferences({ password: op }, { password: { manager: "bitwarden", item: "x" } }),
    ).toBe(false);
  });
});

describe("a second Vault reference", () => {
  it("starts at the secret another slot reads, with its own field", () => {
    const like = vaultBeside({ privateKey: kv, password: op }, "passphrase");
    expect(like).toEqual(kv);
    expect(blankReference("vault", like)).toEqual({ ...kv, field: "" });
  });

  it("starts blank when no other slot reads Vault, or for another manager", () => {
    expect(vaultBeside({ privateKey: kv }, "privateKey")).toBeUndefined();
    expect(vaultBeside({ password: op }, "passphrase")).toBeUndefined();
    expect(blankReference("vault", op)).toEqual(blankReference("vault"));
    expect(blankReference("bitwarden", kv)).toEqual({ manager: "bitwarden", item: "" });
  });
});

describe("what saving asks about", () => {
  it("asks only for a reference that is new or changed, never for one removed", () => {
    expect(addsReferences({ password: op }, {})).toBe(true);
    expect(addsReferences({ password: op }, { password: op })).toBe(false);
    expect(addsReferences({}, { password: op })).toBe(false);
    expect(addsReferences({ passphrase: op }, { password: op })).toBe(true);
    expect(addsReferences({ privateKey: { ...kv, field: "other" } }, { privateKey: kv })).toBe(
      true,
    );
  });
});
