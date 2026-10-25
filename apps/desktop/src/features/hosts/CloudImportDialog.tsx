// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useEffect, useId, useRef, useState } from "react";

import type { CloudAsk, CloudPreview } from "@/ipc/index.ts";

import { type HostsStore, messageOf } from "./hosts-store.ts";

type Provider = CloudAsk["provider"];

/** Each provider, its CLI, and what its scope is called there. */
const PROVIDERS: readonly { value: Provider; label: string; cli: string; scope: string }[] = [
  { value: "ec2", label: "AWS EC2", cli: "aws", scope: "Profile" },
  { value: "lightsail", label: "AWS Lightsail", cli: "aws", scope: "Profile" },
  { value: "digitalOcean", label: "DigitalOcean", cli: "doctl", scope: "Context" },
  { value: "azure", label: "Azure", cli: "az", scope: "Subscription" },
  { value: "gcp", label: "Google Cloud", cli: "gcloud", scope: "Project" },
];

/**
 * Hosts from a cloud account (ADR 0032): the provider's CLI, signed in as
 * the user is, lists them; the user sees what an import would do, and
 * nothing is saved until they accept it.
 */
export function CloudImportDialog({ store, onClose }: { store: HostsStore; onClose: () => void }) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [provider, setProvider] = useState<Provider>("ec2");
  const [scope, setScope] = useState("");
  const [address, setAddress] = useState<CloudAsk["address"]>("public");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>();
  const [preview, setPreview] = useState<CloudPreview | undefined>();
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);
  const chosen = PROVIDERS.find((p) => p.value === provider) ?? PROVIDERS[0];

  const list = (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(undefined);
    setPreview(undefined);
    store
      .cloudPreview({ provider, scope: scope.trim() || null, address })
      .then(setPreview)
      .catch((failure: unknown) => setError(messageOf(failure)))
      .finally(() => setBusy(false));
  };

  const accept = () => {
    if (!preview || !chosen) return;
    setBusy(true);
    store
      .cloudImport(preview.token, chosen.label)
      .then(onClose)
      .catch((failure: unknown) => setError(messageOf(failure)))
      .finally(() => setBusy(false));
  };

  const changes = preview
    ? preview.counts.add + preview.counts.moved + preview.counts.back + preview.counts.gone
    : 0;

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className="panel modal w-[min(560px,90vw)] p-0"
    >
      <form onSubmit={list} className="space-y-3 p-4">
        <h2 id={titleId} className="text-base font-semibold">
          Import from a cloud account
        </h2>
        <p className="text-sm opacity-80">
          Tauminus runs the provider's own command-line tool, signed in as you are. It keeps no
          cloud key. You see what would change before anything is saved.
        </p>
        <label className="block text-sm">
          Provider
          <select
            value={provider}
            onChange={(e) => {
              const picked = PROVIDERS.find((p) => p.value === e.target.value);
              if (picked) setProvider(picked.value);
              setPreview(undefined);
            }}
            className="field mt-1 w-full"
          >
            {PROVIDERS.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label} ({p.cli})
              </option>
            ))}
          </select>
        </label>
        <label className="block text-sm">
          {chosen?.scope ?? "Scope"} (blank for the CLI's own default)
          <input
            value={scope}
            onChange={(e) => setScope(e.target.value)}
            spellCheck={false}
            className="field mt-1 w-full"
          />
        </label>
        <fieldset className="text-sm">
          <legend>Address</legend>
          {(["public", "private"] as const).map((choice) => (
            <label key={choice} className="mr-4 inline-flex items-center gap-1">
              <input
                type="radio"
                name="address"
                checked={address === choice}
                onChange={() => {
                  setAddress(choice);
                  setPreview(undefined);
                }}
              />
              {choice === "public" ? "Public" : "Private (through a VPN or jump host)"}
            </label>
          ))}
        </fieldset>
        {error && (
          <p role="alert" className="text-sm text-error">
            {error}
          </p>
        )}
        {preview && (
          <section aria-label="What the import would do" className="space-y-2 text-sm">
            <p className="text-xs opacity-70">
              Ran {preview.program || chosen?.cli} for {preview.account}.
            </p>
            <Listed
              title={`Add ${preview.counts.add}`}
              items={preview.add.map((p) => `${p.label} ${p.address} (${p.region})`)}
              more={preview.counts.add - preview.add.length}
            />
            <Listed
              title={`Address moved ${preview.counts.moved}`}
              items={preview.moved.map((m) => `${m.label}: ${m.from} → ${m.to}`)}
              more={preview.counts.moved - preview.moved.length}
            />
            <Listed
              title={`Back in the account ${preview.counts.back}`}
              items={preview.back}
              more={preview.counts.back - preview.back.length}
            />
            <Listed
              title={`Gone from the account ${preview.counts.gone} (tagged cloud-gone, not deleted)`}
              items={preview.gone}
              more={preview.counts.gone - preview.gone.length}
            />
            <Listed
              title={`Skipped ${preview.counts.skipped}`}
              items={preview.skipped.map((s) => `${s.name}: ${s.reason}`)}
              more={preview.counts.skipped - preview.skipped.length}
            />
          </section>
        )}
        <div className="flex justify-end gap-2">
          <button type="button" className="btn" onClick={onClose}>
            Close
          </button>
          <button type="submit" className="btn" disabled={busy}>
            {busy && !preview ? "Listing…" : "List"}
          </button>
          {preview && (
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || changes === 0}
              onClick={accept}
            >
              Import
            </button>
          )}
        </div>
      </form>
    </dialog>
  );
}

function Listed({ title, items, more }: { title: string; items: string[]; more: number }) {
  if (items.length === 0) return null;
  return (
    <details open={items.length <= 10}>
      <summary>{title}</summary>
      <ul className="ml-4 list-disc font-mono text-xs">
        {items.map((item) => (
          <li key={item}>{item}</li>
        ))}
        {more > 0 && <li>and {more} more</li>}
      </ul>
    </details>
  );
}
