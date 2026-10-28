// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The fields for one slot's source: this vault, or a password manager.

import type { ReactNode } from "react";

import type { SecretRefSpec } from "@/ipc/index.ts";

import { blankReference, MANAGERS, managersFor, type Slot } from "./references.ts";

const FIELD = "field";

/**
 * One slot's source. With "This vault" chosen it shows `children`, the
 * fields that store the secret here; with a manager, that manager's fields.
 */
export function SlotSource({
  label,
  slot,
  reference,
  like,
  onChange,
  children,
}: {
  label: string;
  slot: Slot;
  reference: SecretRefSpec | null | undefined;
  /** Another slot's reference, which a new one may start from. */
  like?: SecretRefSpec | undefined;
  onChange: (next: SecretRefSpec | null) => void;
  children: ReactNode;
}) {
  return (
    <fieldset aria-label={`${label} source`} className="mb-1">
      <label className="mb-3 block">
        <span className="mb-1.5 block text-[13px] font-medium text-muted">{`${label} from`}</span>
        <select
          value={reference?.manager ?? "here"}
          onChange={(e) => {
            const chosen = MANAGERS.find((m) => m.value === e.target.value);
            onChange(chosen ? blankReference(chosen.value, like) : null);
          }}
          className={FIELD}
        >
          <option value="here">This vault</option>
          {managersFor(slot).map(({ value, label: name }) => (
            <option key={value} value={value}>
              {name}
            </option>
          ))}
        </select>
      </label>
      {reference ? <ManagerFields reference={reference} onChange={onChange} /> : children}
    </fieldset>
  );
}

function ManagerFields({
  reference,
  onChange,
}: {
  reference: SecretRefSpec;
  onChange: (next: SecretRefSpec) => void;
}) {
  switch (reference.manager) {
    case "onePassword":
      return (
        <Text
          label="Secret reference"
          value={reference.reference}
          placeholder="op://vault/item/field"
          onChange={(value) => onChange({ ...reference, reference: value })}
          hint={
            <>
              1Password's Copy Secret Reference. Read with <code>op read</code> when a host asks for
              it.
            </>
          }
        />
      );
    case "bitwarden":
      return (
        <Text
          label="Item id"
          value={reference.item}
          placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
          onChange={(value) => onChange({ ...reference, item: value })}
          hint={
            <>
              The item's password, read with <code>bw get password</code>. Start Tauminus from a
              shell where <code>bw unlock</code> set <code>BW_SESSION</code>.
            </>
          }
        />
      );
    default:
      return (
        <>
          <Text
            label="Vault address"
            value={reference.address}
            placeholder="https://vault.example.com:8200"
            onChange={(value) => onChange({ ...reference, address: value })}
          />
          <Text
            label="KV mount"
            value={reference.mount}
            onChange={(value) => onChange({ ...reference, mount: value })}
          />
          <Text
            label="Secret path"
            value={reference.path}
            placeholder="ssh/deploy"
            onChange={(value) => onChange({ ...reference, path: value })}
          />
          <Text
            label="Field"
            value={reference.field}
            onChange={(value) => onChange({ ...reference, field: value })}
            hint={
              <>
                Read with <code>vault kv get</code> and the token <code>vault login</code> keeps;
                that token goes to this address.
              </>
            }
          />
        </>
      );
  }
}

function Text({
  label,
  value,
  placeholder,
  onChange,
  hint,
}: {
  label: string;
  value: string;
  placeholder?: string;
  onChange: (value: string) => void;
  hint?: ReactNode;
}) {
  return (
    <label className="mb-3 block">
      <span className="mb-1.5 block text-[13px] font-medium text-muted">{label}</span>
      <input
        required
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => onChange(e.target.value)}
        className={FIELD}
      />
      {hint && <span className="mt-1 block text-xs opacity-70">{hint}</span>}
    </label>
  );
}
