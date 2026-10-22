// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FormEvent,
  memo,
  type ReactNode,
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

import { PromptDialog } from "@/features/ssh/PromptDialog.tsx";
import type { AnswerSpec, HostView, IdentityView, KeyKindSpec, PromptKind } from "@/ipc/index.ts";
import { messageOf } from "@/features/hosts/hosts-store.ts";

import type { VaultStore } from "./vault-store.ts";

/** Moves focus to `ref` once, when its view appears. */
function useInitialFocus(ref: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    ref.current?.focus();
  }, [ref]);
}

const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1.5 outline-none focus:border-current/50";

/** Auto-lock choices, in minutes. */
const IDLE_CHOICES = [1, 5, 15, 30, 60, 240, 1440];

/** What the identity editor offers for a public key: copying it, and
 * installing it on a saved host. */
export interface KeyTools {
  hosts: readonly HostView[];
  copy: (text: string) => void;
}

interface VaultDialogProps {
  store: VaultStore;
  tools: KeyTools;
  onClose: () => void;
  /** Called once the vault is unlocked, for a connect waiting on it. */
  onUnlocked?: () => void;
}

/** Sets up, unlocks, and manages the vault. A modal dialog: Escape closes it. */
export function VaultDialog({ store, tools, onClose, onUnlocked }: VaultDialogProps) {
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
    void store.refresh().catch((error: unknown) => console.warn("vault status failed", error));
  }, [store]);
  const status = state.status;
  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className="top-[8%] m-0 mx-auto max-h-[84vh] w-[min(640px,92vw)] overflow-auto rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl backdrop:bg-black/30"
    >
      <h2 id={titleId} className="mb-3 font-semibold">
        Vault
      </h2>
      {!status ? (
        <p className="opacity-70">Reading the vault…</p>
      ) : !status.exists ? (
        <SetUp store={store} onDone={onUnlocked} />
      ) : !status.unlocked ? (
        <Unlock
          store={store}
          keychain={
            status.keychain.kind !== "on" ||
            // The check cannot run here: only the password unlocks.
            (status.keychain.presence && status.presence.unavailable !== null)
              ? undefined
              : status.keychain.presence
                ? `Unlock with ${status.presence.name}`
                : "Unlock with the system keychain"
          }
          onDone={onUnlocked}
        />
      ) : (
        <Unlocked store={store} identities={state.identities} tools={tools} />
      )}
      <div className="mt-4 flex justify-end">
        <button
          type="button"
          onClick={() => dialogRef.current?.close()}
          className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
        >
          Close
        </button>
      </div>
    </dialog>
  );
}

function SetUp({ store, onDone }: { store: VaultStore; onDone: (() => void) | undefined }) {
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | undefined>();
  const first = useRef<HTMLInputElement>(null);
  useInitialFocus(first);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (password !== confirm) {
      setError("The passwords do not match");
      return;
    }
    store
      .create(password)
      .then(() => {
        setPassword("");
        setConfirm("");
        onDone?.();
      })
      .catch((failure: unknown) => setError(messageOf(failure)));
  };
  return (
    <form onSubmit={submit}>
      <p className="mb-3 opacity-80">
        The vault keeps passwords and keys encrypted with a master password. Nobody, Tauminus
        included, can open it without that password, so choose one you will remember.
      </p>
      <Field label="Master password">
        <input
          type="password"
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className={FIELD}
          ref={first}
        />
      </Field>
      <Field label="Repeat it">
        <input
          type="password"
          autoComplete="new-password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
          className={FIELD}
        />
      </Field>
      <Problem text={error} />
      <Submit>Create vault</Submit>
    </form>
  );
}

function Unlock({
  store,
  keychain,
  onDone,
}: {
  store: VaultStore;
  /** The keychain button's label, when the keychain unlocks the vault. */
  keychain: string | undefined;
  onDone: (() => void) | undefined;
}) {
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | undefined>();
  // A system prompt is open: one at a time.
  const [asking, setAsking] = useState(false);
  const first = useRef<HTMLInputElement>(null);
  useInitialFocus(first);
  const unlocked = () => {
    setPassword("");
    onDone?.();
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    store
      .unlock(password)
      .then(unlocked)
      .catch((failure: unknown) => setError(messageOf(failure)));
  };
  return (
    <form onSubmit={submit}>
      <Field label="Master password">
        <input
          type="password"
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className={FIELD}
          ref={first}
        />
      </Field>
      <Problem text={error} />
      <div className="flex gap-2">
        <Submit>Unlock</Submit>
        {keychain && (
          <button
            type="button"
            disabled={asking}
            onClick={() => {
              setAsking(true);
              store
                .unlockWithKeychain()
                .then(unlocked)
                .catch((failure: unknown) => setError(messageOf(failure)))
                .finally(() => setAsking(false));
            }}
            className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
          >
            {keychain}
          </button>
        )}
      </div>
    </form>
  );
}

function Unlocked({
  store,
  identities,
  tools,
}: {
  store: VaultStore;
  identities: readonly IdentityView[];
  tools: KeyTools;
}) {
  const [editing, setEditing] = useState<IdentityView | "new" | undefined>();
  const [error, setError] = useState<string | undefined>();
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const status = state.status;
  return (
    <div>
      <div className="mb-3 flex items-center gap-2">
        <h3 className="flex-1 font-medium">Identities</h3>
        <button
          type="button"
          onClick={() => setEditing("new")}
          className="rounded border border-current/20 px-2 py-1 hover:bg-current/10"
        >
          New identity
        </button>
        <button
          type="button"
          onClick={() => {
            store.lock().catch((failure: unknown) => setError(messageOf(failure)));
          }}
          className="rounded border border-current/20 px-2 py-1 hover:bg-current/10"
        >
          Lock now
        </button>
      </div>
      {editing ? (
        <IdentityForm
          store={store}
          identity={editing === "new" ? undefined : editing}
          tools={tools}
          onDone={() => setEditing(undefined)}
        />
      ) : identities.length === 0 ? (
        <p className="mb-3 opacity-70">
          No identities yet. An identity is a user, a password, and a key that several hosts can
          share.
        </p>
      ) : (
        <ul
          aria-label="Identities"
          className="mb-3 divide-y divide-current/10 rounded border border-current/15"
        >
          {identities.map((identity) => (
            <li key={identity.id} className="flex items-center gap-2 px-3 py-2">
              <div className="min-w-0 flex-1">
                <div className="font-medium">{identity.name}</div>
                <div className="truncate text-xs opacity-70">
                  {[
                    identity.user,
                    identity.hasPassword ? "password" : undefined,
                    identity.hasKey ? "key" : undefined,
                  ]
                    .filter(Boolean)
                    .join(" · ") || "nothing stored"}
                </div>
              </div>
              <button
                type="button"
                aria-label={`Edit ${identity.name}`}
                onClick={() => setEditing(identity)}
                className="rounded px-2 py-1 hover:bg-current/10"
              >
                Edit
              </button>
            </li>
          ))}
        </ul>
      )}
      <Problem text={error} />
      {status && <Settings store={store} status={status} />}
    </div>
  );
}

/** Where the identity's key comes from on save. */
const KEY_SOURCES: readonly { value: "paste" | KeyKindSpec; label: string }[] = [
  { value: "paste", label: "Paste an OpenSSH private key" },
  { value: "ed25519", label: "Generate Ed25519 (recommended)" },
  { value: "ecdsa-p256", label: "Generate ECDSA P-256" },
  { value: "ecdsa-p384", label: "Generate ECDSA P-384" },
  { value: "ecdsa-p521", label: "Generate ECDSA P-521" },
  { value: "rsa3072", label: "Generate RSA 3072" },
  { value: "rsa4096", label: "Generate RSA 4096 (slow)" },
];

function IdentityForm({
  store,
  identity,
  tools,
  onDone,
}: {
  store: VaultStore;
  identity: IdentityView | undefined;
  tools: KeyTools;
  onDone: () => void;
}) {
  const [source, setSource] = useState<"paste" | KeyKindSpec>("paste");
  const [saving, setSaving] = useState(false);
  const [name, setName] = useState(identity?.name ?? "");
  const [user, setUser] = useState(identity?.user ?? "");
  const [password, setPassword] = useState("");
  const [privateKey, setPrivateKey] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [removePassword, setRemovePassword] = useState(false);
  const [removeKey, setRemoveKey] = useState(false);
  const [error, setError] = useState<string | undefined>();
  const [confirming, setConfirming] = useState(false);
  const first = useRef<HTMLInputElement>(null);
  useInitialFocus(first);
  // Removing the key wins over a generate choice left in the select.
  const generate = removeKey || source === "paste" ? null : source;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(undefined);
    setSaving(true);
    store
      .saveIdentity({
        generate,
        id: identity?.id ?? null,
        name,
        user: user || null,
        // Blank keeps what is stored, "" removes it; the fields never show
        // a stored secret.
        password: removePassword ? "" : password || null,
        privateKey: removeKey ? "" : privateKey || null,
        passphrase: removeKey ? "" : passphrase || null,
      })
      .then(onDone)
      .catch((failure: unknown) => setError(messageOf(failure)))
      .finally(() => setSaving(false));
  };
  return (
    <>
      <form
        onSubmit={submit}
        aria-label={identity ? `Edit ${identity.name}` : "New identity"}
        className="mb-3 rounded border border-current/15 p-3"
      >
        <Field label="Name">
          <input
            required
            value={name}
            onChange={(e) => setName(e.target.value)}
            className={FIELD}
            ref={first}
          />
        </Field>
        <Field label="User">
          <input
            value={user}
            onChange={(e) => setUser(e.target.value)}
            spellCheck={false}
            className={FIELD}
          />
        </Field>
        <Field label={identity?.hasPassword ? "Password (stored; type to replace)" : "Password"}>
          <input
            type="password"
            autoComplete="off"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            disabled={removePassword}
            className={FIELD}
          />
        </Field>
        {identity?.hasPassword && (
          <Remove checked={removePassword} onChange={setRemovePassword}>
            Remove the stored password
          </Remove>
        )}
        <Field label={identity?.hasKey ? "Key (one is stored)" : "Key"}>
          <select
            value={source}
            onChange={(e) => {
              const chosen = KEY_SOURCES.find((k) => k.value === e.target.value);
              if (chosen) setSource(chosen.value);
            }}
            disabled={removeKey}
            className={FIELD}
          >
            {KEY_SOURCES.map(({ value, label }) => (
              <option key={value} value={value}>
                {identity?.hasKey && value !== "paste"
                  ? `${label}, replacing the stored one`
                  : label}
              </option>
            ))}
          </select>
        </Field>
        {source === "paste" && (
          <Field
            label={
              identity?.hasKey ? "Private key (stored; paste to replace)" : "Private key (OpenSSH)"
            }
          >
            <textarea
              value={privateKey}
              onChange={(e) => setPrivateKey(e.target.value)}
              disabled={removeKey}
              rows={3}
              spellCheck={false}
              placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
              className={`${FIELD} font-mono text-xs`}
            />
          </Field>
        )}
        {identity?.hasKey && (
          <Remove checked={removeKey} onChange={setRemoveKey}>
            Remove the stored key and its passphrase
          </Remove>
        )}
        {source === "paste" && (
          <Field label="Key passphrase">
            <input
              type="password"
              autoComplete="off"
              value={passphrase}
              onChange={(e) => setPassphrase(e.target.value)}
              className={FIELD}
            />
          </Field>
        )}
        <Problem text={error} />
        <div className="flex gap-2">
          {identity && (
            <button
              type="button"
              onClick={() => {
                if (!confirming) {
                  setConfirming(true);
                  return;
                }
                store
                  .deleteIdentity(identity.id)
                  .then(onDone)
                  .catch((failure: unknown) => setError(messageOf(failure)));
              }}
              className="rounded border border-red-500/50 px-3 py-1.5 text-red-600 hover:bg-red-500/10 dark:text-red-400"
            >
              {confirming ? "Confirm delete" : "Delete"}
            </button>
          )}
          <span className="flex-1" />
          <button
            type="button"
            onClick={onDone}
            className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
          >
            Cancel
          </button>
          <Submit disabled={saving}>{saving && generate ? "Generating…" : "Save"}</Submit>
        </div>
      </form>
      {identity?.publicKey && (
        // Outside the form: the prompts it shows have forms of their own.
        <PublicKey store={store} identity={identity} publicKey={identity.publicKey} tools={tools} />
      )}
    </>
  );
}

type InstallState =
  | { kind: "idle" }
  | { kind: "busy" }
  | { kind: "done"; label: string }
  | { kind: "failed"; text: string };

/**
 * A stored key's public half: copy it, or add it to a saved host. Memoized:
 * its host list can hold thousands of options, and typing in the editor
 * above it should not rebuild them.
 */
const PublicKey = memo(function PublicKey({
  store,
  identity,
  publicKey,
  tools,
}: {
  store: VaultStore;
  identity: IdentityView;
  publicKey: string;
  tools: KeyTools;
}) {
  const [host, setHost] = useState("");
  const [state, setState] = useState<InstallState>({ kind: "idle" });
  const [prompt, setPrompt] = useState<
    { prompt: PromptKind; answer: (answer: AnswerSpec) => void } | undefined
  >();
  // A prompt left open when this closes is declined, so the connect ends
  // instead of waiting for its timeout.
  const pending = useRef<((answer: AnswerSpec) => void) | undefined>(undefined);
  useEffect(() => () => pending.current?.({ kind: "cancel" }), []);
  const install = () => {
    const target = tools.hosts.find((h) => h.id === host);
    if (!target) return;
    setState({ kind: "busy" });
    store
      .installKey(
        identity.id,
        target.id,
        (asked) =>
          new Promise<AnswerSpec>((resolve) => {
            pending.current = resolve;
            setPrompt({
              prompt: asked,
              answer: (answer) => {
                pending.current = undefined;
                setPrompt(undefined);
                resolve(answer);
              },
            });
          }),
      )
      .then(() => setState({ kind: "done", label: target.label }))
      .catch((failure: unknown) => setState({ kind: "failed", text: messageOf(failure) }))
      // A prompt still shown when the install ends answers nothing now.
      .finally(() => {
        pending.current = undefined;
        setPrompt(undefined);
      });
  };
  return (
    <div className="relative mb-3 rounded border border-current/15 p-2 text-xs">
      <div className="mb-2 flex items-start gap-2">
        <code className="min-w-0 flex-1 break-all select-all">{publicKey}</code>
        <button
          type="button"
          onClick={() => tools.copy(publicKey)}
          className="rounded border border-current/20 px-2 py-1 hover:bg-current/10"
        >
          Copy public key
        </button>
      </div>
      <div className="flex items-center gap-2">
        <select
          aria-label="Host to install the key on"
          value={host}
          onChange={(e) => setHost(e.target.value)}
          className={`${FIELD} flex-1`}
        >
          <option value="">Choose a saved host…</option>
          {tools.hosts.map((h) => (
            <option key={h.id} value={h.id}>
              {h.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          disabled={!host || state.kind === "busy"}
          onClick={install}
          className="rounded border border-current/20 px-2 py-1 hover:bg-current/10 disabled:opacity-50"
        >
          {state.kind === "busy" ? "Installing…" : "Install on host"}
        </button>
      </div>
      {state.kind === "done" && (
        <output className="mt-2 block">The key is in authorized_keys on {state.label}.</output>
      )}
      {state.kind === "failed" && (
        <p role="alert" className="mt-2 text-red-600 dark:text-red-400">
          {state.text}
        </p>
      )}
      {prompt && <PromptDialog prompt={prompt.prompt} onAnswer={prompt.answer} />}
    </div>
  );
});

function Settings({
  store,
  status,
}: {
  store: VaultStore;
  status: NonNullable<ReturnType<VaultStore["getSnapshot"]>["status"]>;
}) {
  const [error, setError] = useState<string | undefined>();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [changed, setChanged] = useState<string | undefined>();
  const [enabling, setEnabling] = useState(false);
  const [confirmPassword, setConfirmPassword] = useState("");
  // A system prompt for the presence setting is open: one at a time.
  const [asking, setAsking] = useState(false);
  const keychain = status.keychain;
  const report = (failure: unknown) => setError(messageOf(failure));
  // Each action starts with no message left over from the one before.
  const begin = () => {
    setError(undefined);
    setChanged(undefined);
  };
  return (
    <details className="rounded border border-current/15 px-3 py-2">
      <summary className="cursor-pointer select-none">Settings</summary>
      <div className="mt-2">
        <Field label="Lock after this long unused">
          <select
            value={status.idleMinutes}
            onChange={(e) => {
              begin();
              store.setIdle(Number(e.target.value)).catch(report);
            }}
            className={FIELD}
          >
            {IDLE_CHOICES.map((minutes) => (
              <option key={minutes} value={minutes}>
                {minutes < 60 ? `${minutes} min` : `${minutes / 60} h`}
              </option>
            ))}
          </select>
        </Field>
        <label className="mb-1 flex items-center gap-2">
          <input
            type="checkbox"
            checked={keychain.kind === "on" || enabling}
            disabled={keychain.kind === "unavailable"}
            onChange={(e) => {
              if (e.target.checked) {
                // From then on the vault opens without the password, so ask
                // for it first.
                setEnabling(true);
                return;
              }
              if (enabling) {
                setEnabling(false);
                return;
              }
              begin();
              store.setKeychain(false).catch(report);
            }}
          />
          <span>Let the system keychain unlock the vault</span>
        </label>
        {enabling && (
          <div className="mb-2 flex items-end gap-2">
            <label className="flex-1">
              <span className="mb-1 block text-xs opacity-80">
                Master password, to turn this on
              </span>
              <input
                type="password"
                autoComplete="current-password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className={FIELD}
              />
            </label>
            <button
              type="button"
              onClick={() => {
                begin();
                store
                  .setKeychain(true, confirmPassword)
                  .then(() => {
                    setEnabling(false);
                    setConfirmPassword("");
                  })
                  .catch((failure: unknown) => {
                    setEnabling(false);
                    report(failure);
                  });
              }}
              className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
            >
              Turn on
            </button>
          </div>
        )}
        <p className="mb-3 text-xs opacity-60">
          {keychain.kind === "unavailable"
            ? `Unavailable: ${keychain.reason}`
            : "Anyone who can open your keychain can then open the vault."}
        </p>
        {keychain.kind === "on" && (
          <>
            <label className="mb-1 flex items-center gap-2">
              <input
                type="checkbox"
                checked={keychain.presence}
                disabled={asking || status.presence.unavailable !== null}
                onChange={(e) => {
                  begin();
                  setAsking(true);
                  store
                    .setPresence(e.target.checked)
                    .catch(report)
                    .finally(() => setAsking(false));
                }}
              />
              <span>Ask for {status.presence.name} before the keychain unlocks the vault</span>
            </label>
            <p className="mb-3 text-xs opacity-60">
              {status.presence.unavailable === null
                ? `${status.presence.name} confirms it is you before Tauminus uses its keychain key. Other programs running as you can still read that key.`
                : keychain.presence
                  ? `Unavailable: ${status.presence.unavailable}. Turn the keychain off to remove this check; the master password still unlocks.`
                  : `Unavailable: ${status.presence.unavailable}`}
            </p>
          </>
        )}
        <form
          onSubmit={(event) => {
            event.preventDefault();
            begin();
            store
              .changePassword(current, next)
              .then((keychainOff) => {
                setCurrent("");
                setNext("");
                setChanged(
                  keychainOff === null
                    ? "The master password changed."
                    : `The master password changed. The system keychain was turned off: ${keychainOff}`,
                );
              })
              .catch(report);
          }}
        >
          <Field label="Current master password">
            <input
              type="password"
              autoComplete="current-password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
              className={FIELD}
            />
          </Field>
          <Field label="New master password">
            <input
              type="password"
              autoComplete="new-password"
              value={next}
              onChange={(e) => setNext(e.target.value)}
              className={FIELD}
            />
          </Field>
          {changed && <output className="mb-2 block text-xs">{changed}</output>}
          <Problem text={error} />
          <Submit>Change password</Submit>
        </form>
      </div>
    </details>
  );
}

function Remove({
  checked,
  onChange,
  children,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  children: ReactNode;
}) {
  return (
    <label className="-mt-2 mb-3 flex items-center gap-2 text-xs">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span>{children}</span>
    </label>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="mb-3 block">
      <span className="mb-1 block opacity-80">{label}</span>
      {children}
    </label>
  );
}

function Problem({ text }: { text: string | undefined }) {
  return text ? (
    <p role="alert" className="mb-3 text-red-600 dark:text-red-400">
      {text}
    </p>
  ) : null;
}

function Submit({ children, disabled }: { children: ReactNode; disabled?: boolean }) {
  return (
    <button
      type="submit"
      disabled={disabled}
      className="rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90"
    >
      {children}
    </button>
  );
}
