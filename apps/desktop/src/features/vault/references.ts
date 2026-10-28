// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Where an identity's secret comes from: this vault, or a password manager
// read through its own CLI at connect time (ADR 0035). The backend checks
// every reference and asks the user in a system dialog before a new one is
// saved; the view only collects the fields.

import type { SecretRefSpec, SecretRefsSpec } from "@/ipc/index.ts";

export type Slot = keyof SecretRefsSpec;
export type Manager = SecretRefSpec["manager"];

export const MANAGERS: readonly { value: Manager; label: string }[] = [
  { value: "onePassword", label: "1Password" },
  { value: "bitwarden", label: "Bitwarden" },
  { value: "vault", label: "HashiCorp Vault" },
];

/** Bitwarden's CLI prints a password, never a key file. */
export function managersFor(slot: Slot): readonly { value: Manager; label: string }[] {
  return slot === "privateKey" ? MANAGERS.filter((m) => m.value !== "bitwarden") : MANAGERS;
}

export function managerName(manager: Manager): string {
  return MANAGERS.find((m) => m.value === manager)?.label ?? manager;
}

/**
 * A new reference for `manager`. A Vault one starts where `like`, another
 * slot's Vault reference, points: a key and its passphrase usually sit in
 * one secret, under two fields.
 */
export function blankReference(manager: Manager, like?: SecretRefSpec | null): SecretRefSpec {
  if (manager === "vault" && like?.manager === "vault") {
    return { manager, address: like.address, mount: like.mount, path: like.path, field: "" };
  }
  switch (manager) {
    case "onePassword":
      return { manager, reference: "" };
    case "bitwarden":
      return { manager, item: "" };
    default:
      return { manager, address: "", mount: "secret", path: "", field: "" };
  }
}

const SLOTS: readonly Slot[] = ["password", "privateKey", "passphrase"];

/** The slots that hold a reference, with nothing else in the object. */
export function setReferences(refs: SecretRefsSpec): SecretRefsSpec {
  const out: SecretRefsSpec = {};
  for (const slot of SLOTS) {
    const ref = refs[slot];
    if (ref) out[slot] = ref;
  }
  return out;
}

export function hasReferences(refs: SecretRefsSpec): boolean {
  return SLOTS.some((slot) => Boolean(refs[slot]));
}

function sameReference(a: SecretRefSpec | null | undefined, b: SecretRefSpec | null | undefined) {
  if (!a || !b) return !a && !b;
  switch (a.manager) {
    case "onePassword":
      return b.manager === a.manager && b.reference === a.reference;
    case "bitwarden":
      return b.manager === a.manager && b.item === a.item;
    default:
      return (
        b.manager === a.manager &&
        b.address === a.address &&
        b.mount === a.mount &&
        b.path === a.path &&
        b.field === a.field
      );
  }
}

export function sameReferences(a: SecretRefsSpec, b: SecretRefsSpec): boolean {
  return SLOTS.every((slot) => sameReference(a[slot], b[slot]));
}

/** Whether `next` holds a reference that `was` did not: only that asks. */
export function addsReferences(next: SecretRefsSpec, was: SecretRefsSpec): boolean {
  return SLOTS.some((slot) => Boolean(next[slot]) && !sameReference(next[slot], was[slot]));
}

/** Another slot's Vault reference, to start a new one from. */
export function vaultBeside(refs: SecretRefsSpec, slot: Slot): SecretRefSpec | undefined {
  return (
    SLOTS.filter((other) => other !== slot)
      .map((other) => refs[other])
      .find((ref) => ref?.manager === "vault") ?? undefined
  );
}
