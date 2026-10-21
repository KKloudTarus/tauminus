// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, type ReactNode, useEffect, useId, useRef, useState } from "react";

import type { GroupView, HostView, SettingsSpec } from "@/ipc/index.ts";

import { type HostsStore, messageOf } from "./hosts-store.ts";

export type Editing =
  | { kind: "host"; host: HostView | undefined }
  | { kind: "group"; group: GroupView | undefined };

const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1.5 outline-none focus:border-current/50";

/** `a, b ,c` → `["a", "b", "c"]`; blank → `undefined`, meaning inherit. */
function list(text: string): string[] | undefined {
  const items = text
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
  return items.length > 0 ? items : undefined;
}

function port(text: string): number | null | undefined {
  if (text.trim() === "") return undefined;
  const value = Number(text);
  return Number.isInteger(value) ? value : null;
}

/**
 * The form's fields over `base`, keeping settings the form does not edit,
 * such as a route or auth methods. `direct` means no jump hosts, even if a
 * group has some.
 */
export function withEdits(
  base: SettingsSpec,
  fields: { user: string; port: string; jumps: string; direct?: boolean; identityFiles?: string },
): SettingsSpec {
  const settings: SettingsSpec = { ...base };
  settings.user = fields.user.trim() || null;
  settings.port = port(fields.port) ?? null;
  settings.jumps = fields.direct ? [] : (list(fields.jumps) ?? null);
  if (fields.identityFiles !== undefined)
    settings.identityFiles = list(fields.identityFiles) ?? null;
  return settings;
}

function describeRoute(route: NonNullable<SettingsSpec["route"]>): string {
  switch (route.kind) {
    case "proxyCommand":
      return `ProxyCommand ${route.command}`;
    case "socks5":
      return `SOCKS5 proxy ${route.host}:${route.port}`;
    case "httpConnect":
      return `HTTP proxy ${route.host}:${route.port}`;
    default:
      return "a direct connection";
  }
}

/** Groups by their path, `Prod / EU`, for a select. O(G · d). */
export function groupPaths(groups: readonly GroupView[]): { id: string; path: string }[] {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const path = (group: GroupView): string => {
    const names = [group.name];
    let parent = group.parent ? byId.get(group.parent) : undefined;
    for (let depth = 0; parent && depth < 16; depth += 1) {
      names.unshift(parent.name);
      parent = parent.parent ? byId.get(parent.parent) : undefined;
    }
    return names.join(" / ");
  };
  return groups
    .map((g) => ({ id: g.id, path: path(g) }))
    .toSorted((a, b) => a.path.localeCompare(b.path));
}

/** A group and everything below it, which cannot become its parent. */
function descendants(groups: readonly GroupView[], id: string): Set<string> {
  const found = new Set([id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const group of groups) {
      if (group.parent && found.has(group.parent) && !found.has(group.id)) {
        found.add(group.id);
        grew = true;
      }
    }
  }
  return found;
}

export function EditorDialog({
  store,
  editing,
  onClose,
}: {
  store: HostsStore;
  editing: Editing;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);
  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className="top-[10%] m-0 mx-auto w-[min(520px,90vw)] rounded-lg border border-current/15 bg-background p-0 text-sm text-foreground shadow-xl backdrop:bg-black/30"
    >
      {editing.kind === "host" ? (
        <HostForm store={store} host={editing.host} titleId={titleId} onDone={onClose} />
      ) : (
        <GroupForm store={store} group={editing.group} titleId={titleId} onDone={onClose} />
      )}
    </dialog>
  );
}

function HostForm({
  store,
  host,
  titleId,
  onDone,
}: {
  store: HostsStore;
  host: HostView | undefined;
  titleId: string;
  onDone: () => void;
}) {
  const { view } = store.getSnapshot();
  const [label, setLabel] = useState(host?.label ?? "");
  const [address, setAddress] = useState(host?.address ?? "");
  const [user, setUser] = useState(host?.settings.user ?? "");
  const [portText, setPortText] = useState(host?.settings.port?.toString() ?? "");
  const [group, setGroup] = useState(host?.group ?? "");
  const [tags, setTags] = useState(host?.tags.join(", ") ?? "");
  const [jumps, setJumps] = useState(host?.settings.jumps?.join(", ") ?? "");
  const [direct, setDirect] = useState(host?.settings.jumps?.length === 0);
  const [identityFiles, setIdentityFiles] = useState(
    host?.settings.identityFiles?.join(", ") ?? "",
  );
  const [error, setError] = useState<string | undefined>();
  const [saving, setSaving] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (port(portText) === null) {
      setError("Port must be a whole number");
      return;
    }
    setSaving(true);
    try {
      await store.saveHost({
        id: host?.id ?? null,
        label,
        address,
        group: group || null,
        tags: list(tags) ?? [],
        settings: withEdits(host?.settings ?? {}, {
          user,
          port: portText,
          jumps,
          direct,
          identityFiles,
        }),
      });
      onDone();
    } catch (failure) {
      setError(messageOf(failure));
      setSaving(false);
    }
  };

  return (
    <Form
      title={host ? `Edit ${host.label}` : "New host"}
      titleId={titleId}
      error={error}
      saving={saving}
      onSubmit={(event) => {
        void submit(event);
      }}
      onCancel={onDone}
      onDelete={host ? () => store.deleteHost(host.id).then(onDone) : undefined}
    >
      <Field label="Name">
        <input
          required
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          className={FIELD}
        />
      </Field>
      <Field label="Address">
        <input
          required
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          placeholder="db.example.com or 10.0.0.5"
          spellCheck={false}
          className={`${FIELD} font-mono`}
        />
      </Field>
      <div className="grid grid-cols-[1fr_7rem] gap-2">
        <Field label="User (blank inherits)">
          <input
            value={user}
            onChange={(e) => setUser(e.target.value)}
            spellCheck={false}
            className={FIELD}
          />
        </Field>
        <Field label="Port">
          <input
            value={portText}
            onChange={(e) => setPortText(e.target.value)}
            inputMode="numeric"
            placeholder="22"
            className={FIELD}
          />
        </Field>
      </div>
      <Field label="Group">
        <select value={group} onChange={(e) => setGroup(e.target.value)} className={FIELD}>
          <option value="">No group</option>
          {groupPaths(view.groups).map((g) => (
            <option key={g.id} value={g.id}>
              {g.path}
            </option>
          ))}
        </select>
      </Field>
      <Field label="Tags (comma-separated)">
        <input
          value={tags}
          onChange={(e) => setTags(e.target.value)}
          spellCheck={false}
          className={FIELD}
        />
      </Field>
      <Field label="Jump hosts (like ssh -J; blank inherits)">
        <input
          disabled={direct}
          value={jumps}
          onChange={(e) => setJumps(e.target.value)}
          placeholder="user@bastion:22"
          spellCheck={false}
          className={`${FIELD} font-mono`}
        />
      </Field>
      <label className="mb-3 flex items-center gap-2">
        <input type="checkbox" checked={direct} onChange={(e) => setDirect(e.target.checked)} />
        <span>Connect directly, without the group's jump hosts</span>
      </label>
      {host?.settings.route && host.settings.route.kind !== "direct" && (
        <p className="mb-3 text-xs opacity-70">
          Connects through {describeRoute(host.settings.route)}. It came from ~/.ssh/config and is
          kept as it is.
        </p>
      )}
      <Field label="Key files (blank uses ssh-agent and ~/.ssh/id_*)">
        <input
          value={identityFiles}
          onChange={(e) => setIdentityFiles(e.target.value)}
          placeholder="~/.ssh/id_work"
          spellCheck={false}
          className={`${FIELD} font-mono`}
        />
      </Field>
    </Form>
  );
}

function GroupForm({
  store,
  group,
  titleId,
  onDone,
}: {
  store: HostsStore;
  group: GroupView | undefined;
  titleId: string;
  onDone: () => void;
}) {
  const { view } = store.getSnapshot();
  const [name, setName] = useState(group?.name ?? "");
  const [parent, setParent] = useState(group?.parent ?? "");
  const [user, setUser] = useState(group?.settings.user ?? "");
  const [portText, setPortText] = useState(group?.settings.port?.toString() ?? "");
  const [jumps, setJumps] = useState(group?.settings.jumps?.join(", ") ?? "");
  const [error, setError] = useState<string | undefined>();
  const [saving, setSaving] = useState(false);
  const excluded = group ? descendants(view.groups, group.id) : new Set<string>();

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (port(portText) === null) {
      setError("Port must be a whole number");
      return;
    }
    setSaving(true);
    try {
      await store.saveGroup({
        id: group?.id ?? null,
        name,
        parent: parent || null,
        settings: withEdits(group?.settings ?? {}, { user, port: portText, jumps }),
      });
      onDone();
    } catch (failure) {
      setError(messageOf(failure));
      setSaving(false);
    }
  };

  return (
    <Form
      title={group ? `Edit group ${group.name}` : "New group"}
      titleId={titleId}
      error={error}
      saving={saving}
      onSubmit={(event) => {
        void submit(event);
      }}
      onCancel={onDone}
      onDelete={group ? () => store.deleteGroup(group.id).then(onDone) : undefined}
      deleteNote="Its hosts and groups move up a level and keep their settings."
    >
      <Field label="Name">
        <input required value={name} onChange={(e) => setName(e.target.value)} className={FIELD} />
      </Field>
      <Field label="Inside">
        <select value={parent} onChange={(e) => setParent(e.target.value)} className={FIELD}>
          <option value="">Top level</option>
          {groupPaths(view.groups)
            .filter((g) => !excluded.has(g.id))
            .map((g) => (
              <option key={g.id} value={g.id}>
                {g.path}
              </option>
            ))}
        </select>
      </Field>
      <p className="mb-2 text-xs opacity-60">
        Hosts in this group use these unless they set their own.
      </p>
      <div className="grid grid-cols-[1fr_7rem] gap-2">
        <Field label="User">
          <input
            value={user}
            onChange={(e) => setUser(e.target.value)}
            spellCheck={false}
            className={FIELD}
          />
        </Field>
        <Field label="Port">
          <input
            value={portText}
            onChange={(e) => setPortText(e.target.value)}
            inputMode="numeric"
            className={FIELD}
          />
        </Field>
      </div>
      <Field label="Jump hosts (like ssh -J)">
        <input
          value={jumps}
          onChange={(e) => setJumps(e.target.value)}
          spellCheck={false}
          className={`${FIELD} font-mono`}
        />
      </Field>
    </Form>
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

function Form({
  title,
  titleId,
  error,
  saving,
  onSubmit,
  onCancel,
  onDelete,
  deleteNote,
  children,
}: {
  title: string;
  titleId: string;
  error: string | undefined;
  saving: boolean;
  onSubmit: (event: FormEvent) => void;
  onCancel: () => void;
  onDelete: (() => Promise<void>) | undefined;
  deleteNote?: string;
  children: ReactNode;
}) {
  // Deleting takes a second click, so a slip of the mouse loses nothing.
  const [confirming, setConfirming] = useState(false);
  const [deleteError, setDeleteError] = useState<string | undefined>();
  const shown = deleteError ?? error;
  return (
    <form onSubmit={onSubmit} className="p-4">
      <h2 id={titleId} className="mb-3 font-semibold">
        {title}
      </h2>
      {children}
      {shown && (
        <p role="alert" className="mb-3 text-red-600 dark:text-red-400">
          {shown}
        </p>
      )}
      <div className="flex items-center gap-2">
        {onDelete && (
          <button
            type="button"
            onClick={() => {
              if (!confirming) {
                setConfirming(true);
                return;
              }
              onDelete().catch((failure: unknown) => setDeleteError(messageOf(failure)));
            }}
            title={deleteNote}
            className="rounded border border-red-500/50 px-3 py-1.5 text-red-600 hover:bg-red-500/10 dark:text-red-400"
          >
            {confirming ? "Confirm delete" : "Delete"}
          </button>
        )}
        <span className="flex-1" />
        <button
          type="button"
          onClick={onCancel}
          className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
        >
          Cancel
        </button>
        <button
          type="submit"
          disabled={saving}
          className="rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90 disabled:opacity-50"
        >
          Save
        </button>
      </div>
    </form>
  );
}
