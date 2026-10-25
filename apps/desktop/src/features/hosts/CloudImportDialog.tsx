// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useEffect, useId, useMemo, useRef, useState } from "react";

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
  // The listing asked last: an answer to an earlier one is dropped.
  const asked = useRef(0);
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);
  const chosen = PROVIDERS.find((p) => p.value === provider) ?? PROVIDERS[0];

  /** Any change to what is asked drops the preview of what was. */
  const changed = () => {
    asked.current += 1;
    setPreview(undefined);
  };

  const list = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    const ask = (asked.current += 1);
    setBusy(true);
    setError(undefined);
    setPreview(undefined);
    store
      .cloudPreview({ provider, scope: scope.trim() || null, address })
      .then((shown) => {
        if (ask === asked.current) setPreview(shown);
      })
      .catch((failure: unknown) => {
        if (ask === asked.current) setError(messageOf(failure));
      })
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
  const lists = useMemo(
    () =>
      preview && {
        add: preview.add.map((p) => `${p.label} ${p.address} (${p.region})`),
        moved: preview.moved.map((m) => `${m.label}: ${m.from} → ${m.to}`),
        skipped: preview.skipped.map((s) => `${s.name}: ${s.reason}`),
      },
    [preview],
  );

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
            disabled={busy}
            onChange={(e) => {
              const picked = PROVIDERS.find((p) => p.value === e.target.value);
              if (picked) setProvider(picked.value);
              changed();
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
            disabled={busy}
            onChange={(e) => {
              setScope(e.target.value);
              changed();
            }}
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
                disabled={busy}
                onChange={() => {
                  setAddress(choice);
                  changed();
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
        {preview && lists && (
          <section aria-label="What the import would do" className="space-y-2 text-sm">
            <p className="text-xs opacity-70">
              Ran {preview.program || chosen?.cli} for {preview.account}.
            </p>
            <Listed
              title={`Add ${preview.counts.add}`}
              items={lists.add}
              more={preview.counts.add - preview.add.length}
            />
            <Listed
              title={`Address moved ${preview.counts.moved}`}
              items={lists.moved}
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
              items={lists.skipped}
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
        {items.map((item, at) => (
          // oxlint-disable-next-line react/no-array-index-key -- names repeat; a preview never reorders
          <li key={at}>{item}</li>
        ))}
        {more > 0 && <li>and {more} more</li>}
      </ul>
    </details>
  );
}
