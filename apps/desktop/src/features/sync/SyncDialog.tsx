// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { ReportView, ReviewView, TargetKind } from "@/ipc/index.ts";

import type { SyncStore } from "./sync-store.ts";

const FIELD = "field";

/** At least this many characters, as the backend checks. */
export const MIN_PASSPHRASE = 12;

interface SyncDialogProps {
  store: SyncStore;
  onClose: () => void;
  onOpenVault: () => void;
  /** Hears the hosts each accept let through; a connect waiting on one
   * of them closes the dialog and goes ahead. */
  onReviewed?: (hosts: string[]) => void;
}

/** Sets up sync, shows how it went, and lists what other devices changed. */
export function SyncDialog({ store, onClose, onOpenVault, onReviewed }: SyncDialogProps) {
  const { status, reviews, noted } = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
    void store.refresh().catch((error: unknown) => console.warn("sync status failed", error));
  }, [store]);
  const setup = status?.setup;
  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className="panel modal w-[min(680px,94vw)] p-4"
    >
      <h2 id={titleId} className="mb-3 font-semibold">
        Sync
      </h2>
      {!setup ? (
        <p className="opacity-70">Reading the sync settings…</p>
      ) : setup.kind === "locked" ? (
        <div>
          <p className="mb-3 opacity-80">
            The sync key is kept in the vault. Unlock it, or create one, to set up or run sync.
          </p>
          <button type="button" className="btn btn-primary" onClick={onOpenVault}>
            Open the vault
          </button>
        </div>
      ) : setup.kind === "off" ? (
        <SetUp store={store} />
      ) : (
        <Syncing
          store={store}
          shown={setup.shown}
          git={setup.git}
          running={status.running}
          last={status.last}
        />
      )}
      {reviews.length > 0 && (
        <Reviews
          store={store}
          reviews={reviews}
          noted={noted}
          onAccepted={(hosts) => onReviewed?.(hosts)}
        />
      )}
      <div className="mt-4 flex justify-end">
        <button type="button" onClick={() => dialogRef.current?.close()} className="btn">
          Close
        </button>
      </div>
    </dialog>
  );
}

function SetUp({ store }: { store: SyncStore }) {
  const [kind, setKind] = useState<TargetKind>("folder");
  const [address, setAddress] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [confirm, setConfirm] = useState("");
  const [mode, setMode] = useState<"create" | "join">("create");
  const [error, setError] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    // Characters as the backend counts them: code points.
    if (Array.from(passphrase).length < MIN_PASSPHRASE) {
      setError(`The passphrase needs at least ${MIN_PASSPHRASE} characters`);
      return;
    }
    if (mode === "create" && passphrase !== confirm) {
      setError("The passphrases do not match");
      return;
    }
    setBusy(true);
    setError(undefined);
    const target = { kind, address };
    (mode === "create" ? store.create(target, passphrase) : store.join(target, passphrase))
      .then(() => {
        setPassphrase("");
        setConfirm("");
      })
      .catch((failure: unknown) => setError(messageOf(failure)))
      .finally(() => setBusy(false));
  };
  return (
    <form onSubmit={submit}>
      <p className="mb-3 opacity-80">
        Sync keeps your hosts, groups and snippets the same on each device. Everything is encrypted
        on this device with a key only your passphrase opens; the folder or remote sees ciphertext
        only. Passwords and keys in the vault never sync.
      </p>
      <fieldset className="mb-3 flex gap-4">
        <legend className="mb-1.5 text-[13px] font-medium text-muted">Sync through</legend>
        {(
          [
            ["folder", "A shared folder"],
            ["git", "A git remote"],
          ] as const
        ).map(([value, label]) => (
          <label key={value} className="flex items-center gap-1.5">
            <input
              type="radio"
              name="sync-kind"
              checked={kind === value}
              onChange={() => setKind(value)}
            />
            {label}
          </label>
        ))}
      </fieldset>
      <Field label={kind === "folder" ? "Folder (Syncthing, Dropbox, a network share)" : "Remote"}>
        <span className="flex gap-2">
          <input
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder={
              kind === "folder" ? "/home/me/Sync/tauminus" : "git@github.com:me/sync.git"
            }
            spellCheck={false}
            autoComplete="off"
            className={FIELD}
          />
          {kind === "folder" && (
            <button
              type="button"
              className="btn"
              onClick={() => {
                store
                  .chooseFolder()
                  .then((path) => {
                    if (path !== null) setAddress(path);
                  })
                  .catch((failure: unknown) => setError(messageOf(failure)));
              }}
            >
              Choose…
            </button>
          )}
        </span>
      </Field>
      <fieldset className="mb-3 flex gap-4">
        <legend className="mb-1.5 text-[13px] font-medium text-muted">This device</legend>
        <label className="flex items-center gap-1.5">
          <input
            type="radio"
            name="sync-mode"
            checked={mode === "create"}
            onChange={() => setMode("create")}
          />
          Starts a new sync
        </label>
        <label className="flex items-center gap-1.5">
          <input
            type="radio"
            name="sync-mode"
            checked={mode === "join"}
            onChange={() => setMode("join")}
          />
          Joins one another device started
        </label>
      </fieldset>
      <Field label="Sync passphrase">
        <input
          type="password"
          autoComplete={mode === "create" ? "new-password" : "current-password"}
          value={passphrase}
          onChange={(e) => setPassphrase(e.target.value)}
          className={FIELD}
        />
      </Field>
      {mode === "create" && (
        <Field label="Repeat it">
          <input
            type="password"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            className={FIELD}
          />
        </Field>
      )}
      <p className="mb-3 text-xs opacity-70">
        Every device uses the same passphrase. It is not kept anywhere: lose it, and a new device
        cannot join.
      </p>
      {error && (
        <p role="alert" className="mb-3 text-error">
          {error}
        </p>
      )}
      <button type="submit" disabled={busy || address.trim() === ""} className="btn btn-primary">
        {busy ? "Working…" : mode === "create" ? "Start syncing" : "Join"}
      </button>
    </form>
  );
}

function Syncing({
  store,
  shown,
  git,
  running,
  last,
}: {
  store: SyncStore;
  shown: string;
  git: boolean;
  running: boolean;
  last: ReportView | null;
}) {
  const [error, setError] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [leaving, setLeaving] = useState(false);
  const act = (work: () => Promise<void>) => {
    setBusy(true);
    setError(undefined);
    work()
      .catch((failure: unknown) => setError(messageOf(failure)))
      .finally(() => setBusy(false));
  };
  return (
    <div>
      <p className="mb-1">
        Syncing through {git ? "the git remote" : "the folder"}{" "}
        <code className="font-mono text-xs break-all">{shown}</code>
      </p>
      <LastRun last={last} />
      {error && (
        <p role="alert" className="mb-3 text-error">
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || running}
          onClick={() => act(() => store.now())}
        >
          {busy || running ? "Syncing…" : "Sync now"}
        </button>
        {leaving ? (
          <>
            <button
              type="button"
              className="btn btn-danger"
              disabled={busy}
              onClick={() => act(() => store.leave())}
            >
              Stop syncing here
            </button>
            <button type="button" className="btn" onClick={() => setLeaving(false)}>
              Keep syncing
            </button>
          </>
        ) : (
          <button type="button" className="btn" disabled={busy} onClick={() => setLeaving(true)}>
            Stop syncing on this device…
          </button>
        )}
      </div>
      {leaving && (
        <p className="mt-2 text-xs opacity-70">
          This device keeps its hosts and snippets. The other devices and the target keep theirs.
        </p>
      )}
    </div>
  );
}

function LastRun({ last }: { last: ReportView | null }) {
  if (!last) {
    return <p className="mb-3 text-sm opacity-70">Not synced since this window opened.</p>;
  }
  const at = new Date(last.at ?? 0).toLocaleString();
  return (
    <div className="mb-3 text-sm">
      {last.error === null ? (
        <p className="opacity-80" data-testid="sync-last">
          Last synced {at}.
        </p>
      ) : (
        <p role="alert" className="text-error" data-testid="sync-last">
          The sync at {at} failed: {last.error}
        </p>
      )}
      <List title="Changed on both devices: the later change was kept">
        {last.clashes.map((clash) => (
          <li key={`${clash.record}\u0000${clash.field}`}>
            {clash.record}, {clash.field}: kept <code className="font-mono">{clash.kept}</code>,
            lost <code className="font-mono">{clash.lost}</code>
          </li>
        ))}
      </List>
      <List title="Moved to keep the lists valid">
        {last.repairs.map((repair) => (
          <li key={repair}>{repair}</li>
        ))}
      </List>
      <List title="Notices">
        {last.notices.map((notice) => (
          <li key={notice}>{notice}</li>
        ))}
      </List>
    </div>
  );
}

function List({ title, children }: { title: string; children: ReactNode[] }) {
  if (children.length === 0) return null;
  return (
    <section className="mt-2">
      <h3 className="text-[13px] font-medium text-muted">{title}</h3>
      <ul className="list-disc pl-5">{children}</ul>
    </section>
  );
}

/** Text another device wrote, with what a screen would hide shown: format
 * characters (bidi overrides, zero-width) as `\u{…}` escapes, and the
 * returns startup text types as line breaks. */
export function visible(text: string): string {
  return text
    .replaceAll("\r", "\n")
    .replaceAll(/[\p{Cf}\p{Co}\u2028\u2029]/gu, (c) => `\\u{${c.codePointAt(0)?.toString(16)}}`);
}

function Reviews({
  store,
  reviews,
  noted,
  onAccepted,
}: {
  store: SyncStore;
  reviews: readonly ReviewView[];
  noted: number;
  onAccepted: (hosts: string[]) => void;
}) {
  const [error, setError] = useState<string | undefined>();
  const accept = (shown: readonly ReviewView[]) => {
    setError(undefined);
    store
      .accept(shown)
      .then((stale) => {
        const changed = shown.filter((r) => stale.includes(r.host));
        if (changed.length > 0) {
          const labels = changed.map((r) => visible(r.label)).join(", ");
          setError(`Changed again since shown; check once more: ${labels}`);
        }
        onAccepted(shown.filter((r) => !stale.includes(r.host)).map((r) => r.host));
      })
      .catch((failure: unknown) => setError(messageOf(failure)));
  };
  return (
    <section className="mt-4 border-t border-line pt-3" aria-label="Changes to review">
      <h3 className="mb-1 font-medium">Changes from other devices</h3>
      <p className="mb-3 text-sm opacity-80">
        Another device changed these hosts. Check what connecting to each now does; it is not used
        until you accept it.
        {noted > reviews.length && ` Showing the first ${reviews.length} of up to ${noted}.`}
      </p>
      {error && (
        <p role="alert" className="mb-3 text-error">
          {error}
        </p>
      )}
      {reviews.map((review) => (
        <article
          key={review.host}
          className="mb-3 rounded-md border border-line p-3"
          aria-label={visible(review.label)}
        >
          <h4 className="mb-2 font-medium">
            {visible(review.label)}{" "}
            <span className="text-xs font-normal opacity-70">
              {review.new ? "new from another device" : "changed on another device"}
            </span>
          </h4>
          <table className="mb-2 w-full text-xs">
            <tbody>
              {review.lines.map((line) => (
                <tr key={`${line.level}\u0000${line.field}`} className="align-top">
                  <td className="pr-2 whitespace-nowrap opacity-70">
                    {line.level === 0 ? "host" : `group ${visible(line.place)}`}
                  </td>
                  <td className="pr-2 whitespace-nowrap">{line.field}</td>
                  <td className="font-mono break-all">{visible(line.value)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {review.startup !== "" && (
            <>
              <p className="text-xs opacity-70">
                Typed into each new shell ({review.startup.split("\r").length - 1} lines):
              </p>
              <pre className="mb-2 max-h-40 overflow-auto rounded bg-surface-1 p-2 font-mono text-xs whitespace-pre-wrap">
                {visible(review.startup)}
              </pre>
            </>
          )}
          <button type="button" className="btn btn-primary" onClick={() => accept([review])}>
            Accept {visible(review.label)}
          </button>
        </article>
      ))}
      {reviews.length > 1 && (
        <button type="button" className="btn" onClick={() => accept(reviews)}>
          Accept the {reviews.length} shown
        </button>
      )}
    </section>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="mb-3 block">
      <span className="mb-1.5 block text-[13px] font-medium text-muted">{label}</span>
      {children}
    </label>
  );
}
