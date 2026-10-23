// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, type ReactNode, useEffect, useId, useRef, useState } from "react";

import type {
  EncodingSpec,
  EnvSpec,
  GroupView,
  GuardSpec,
  HostView,
  IdentityView,
  ProtocolSpec,
  SettingsSpec,
  SnippetView,
  TriggerSetView,
} from "@/ipc/index.ts";

import { ForwardFields } from "@/features/forwards/ForwardFields.tsx";
import { ruleRow, rulesToSave } from "@/features/forwards/rules.ts";

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

export type GuardChoice = "inherit" | "on" | "off";

export interface GuardFieldsState {
  enabled: GuardChoice;
  /** One pattern per line. */
  allow: string;
  deny: string;
}

export function guardFields(guard: GuardSpec | null | undefined): GuardFieldsState {
  return {
    enabled: guard?.enabled == null ? "inherit" : guard.enabled ? "on" : "off",
    allow: (guard?.allow ?? []).join("\n"),
    deny: (guard?.deny ?? []).join("\n"),
  };
}

function lines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function guardChoice(value: string): GuardChoice {
  return value === "on" || value === "off" ? value : "inherit";
}

/** The guard fields over `base`; nothing set means no guard settings at all. */
export function withGuard(base: SettingsSpec, fields: GuardFieldsState): SettingsSpec {
  const guard: GuardSpec = {
    enabled: fields.enabled === "inherit" ? null : fields.enabled === "on",
    allow: lines(fields.allow),
    deny: lines(fields.deny),
  };
  const empty = guard.enabled === null && guard.allow?.length === 0 && guard.deny?.length === 0;
  return { ...base, guard: empty ? null : guard };
}

/** The character sets a host's shell may speak, with what each is for. */
const ENCODINGS: readonly (readonly [EncodingSpec, string])[] = [
  ["UTF-8", "UTF-8"],
  ["windows-1252", "Western (Windows-1252, ISO-8859-1)"],
  ["ISO-8859-2", "Central European (ISO-8859-2)"],
  ["windows-1251", "Cyrillic (Windows-1251)"],
  ["KOI8-R", "Cyrillic (KOI8-R)"],
  ["Shift_JIS", "Japanese (Shift_JIS)"],
  ["EUC-JP", "Japanese (EUC-JP)"],
  ["gb18030", "Simplified Chinese (GB18030, GBK)"],
  ["Big5", "Traditional Chinese (Big5)"],
  ["EUC-KR", "Korean (EUC-KR)"],
];

function encodingChoice(value: string): EncodingSpec | "inherit" {
  return ENCODINGS.find(([spec]) => spec === value)?.[0] ?? "inherit";
}

export interface SessionFieldsState {
  forwardAgent: GuardChoice;
  history: GuardChoice;
  /** Whether the AI assistant may read the output. */
  aiOutput: GuardChoice;
  /** The shell's character set, or `inherit`. */
  encoding: EncodingSpec | "inherit";
  /** A tmux session name; `inherit`, or `off` to turn a group's off. */
  tmux: string;
  /** One `NAME=value` per line. */
  env: string;
}

export function sessionFields(settings: SettingsSpec | undefined): SessionFieldsState {
  const agent = settings?.forwardAgent;
  const history = settings?.history;
  const aiOutput = settings?.aiOutput;
  return {
    forwardAgent: agent == null ? "inherit" : agent ? "on" : "off",
    history: history == null ? "inherit" : history ? "on" : "off",
    aiOutput: aiOutput == null ? "inherit" : aiOutput ? "on" : "off",
    encoding: settings?.encoding ?? "inherit",
    tmux: settings?.tmux == null ? "inherit" : settings.tmux === "" ? "off" : settings.tmux,
    env: (settings?.env ?? []).map((v) => `${v.name}=${v.value}`).join("\n"),
  };
}

/**
 * The session fields over `base`. Returns an error message for a line that
 * is not `NAME=value`; the backend checks names and values.
 */
export function withSession(base: SettingsSpec, fields: SessionFieldsState): SettingsSpec | string {
  const env: EnvSpec[] = [];
  // Values keep their spaces; only line endings and blank lines go.
  const entries = fields.env
    .split("\n")
    .map((line) => line.replace(/\r$/u, ""))
    .filter((line) => line.trim() !== "");
  for (const line of entries) {
    const at = line.indexOf("=");
    if (at <= 0) return `"${line}" is not NAME=value`;
    env.push({ name: line.slice(0, at).trim(), value: line.slice(at + 1) });
  }
  return {
    ...base,
    forwardAgent: fields.forwardAgent === "inherit" ? null : fields.forwardAgent === "on",
    history: fields.history === "inherit" ? null : fields.history === "on",
    aiOutput: fields.aiOutput === "inherit" ? null : fields.aiOutput === "on",
    encoding: fields.encoding === "inherit" ? null : fields.encoding,
    tmux:
      fields.tmux.trim() === "" || fields.tmux === "inherit"
        ? null
        : fields.tmux === "off"
          ? ""
          : fields.tmux.trim(),
    env: env.length > 0 ? env : null,
  };
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

const NO_IDENTITIES: readonly IdentityView[] = [];

export function EditorDialog({
  store,
  editing,
  identities = NO_IDENTITIES,
  snippets,
  highlightSets,
  onClose,
}: {
  store: HostsStore;
  editing: Editing;
  identities?: readonly IdentityView[];
  /** Snippets to start with; without them the choice is not shown or saved. */
  snippets?: readonly SnippetView[];
  /** Highlight sets to attach; without them the choice is not shown or saved. */
  highlightSets?: readonly TriggerSetView[];
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
      className="top-[10%] m-0 mx-auto max-h-[80dvh] w-[min(520px,90vw)] overflow-y-auto rounded-lg border border-current/15 bg-background p-0 text-sm text-foreground shadow-xl backdrop:bg-black/30"
    >
      {editing.kind === "host" ? (
        <HostForm
          store={store}
          host={editing.host}
          identities={identities}
          snippets={snippets}
          highlightSets={highlightSets}
          titleId={titleId}
          onDone={onClose}
        />
      ) : (
        <GroupForm
          store={store}
          group={editing.group}
          identities={identities}
          highlightSets={highlightSets}
          titleId={titleId}
          onDone={onClose}
        />
      )}
    </dialog>
  );
}

function HostForm({
  store,
  host,
  identities,
  snippets,
  highlightSets,
  titleId,
  onDone,
}: {
  store: HostsStore;
  host: HostView | undefined;
  identities: readonly IdentityView[];
  snippets: readonly SnippetView[] | undefined;
  highlightSets: readonly TriggerSetView[] | undefined;
  titleId: string;
  onDone: () => void;
}) {
  const [identity, setIdentity] = useState(host?.settings.identity ?? "");
  const [triggers, setTriggers] = useState<readonly string[]>(() => host?.settings.triggers ?? []);
  const { view } = store.getSnapshot();
  const [label, setLabel] = useState(host?.label ?? "");
  const [address, setAddress] = useState(host?.address ?? "");
  const [protocol, setProtocol] = useState<ProtocolSpec>(host?.protocol ?? "ssh");
  const [user, setUser] = useState(host?.settings.user ?? "");
  const [portText, setPortText] = useState(host?.settings.port?.toString() ?? "");
  const [group, setGroup] = useState(host?.group ?? "");
  const [tags, setTags] = useState(host?.tags.join(", ") ?? "");
  const [jumps, setJumps] = useState(host?.settings.jumps?.join(", ") ?? "");
  const [direct, setDirect] = useState(host?.settings.jumps?.length === 0);
  const [identityFiles, setIdentityFiles] = useState(
    host?.settings.identityFiles?.join(", ") ?? "",
  );
  const [guard, setGuard] = useState(() => guardFields(host?.settings.guard));
  const [session, setSession] = useState(() => sessionFields(host?.settings));
  const [rules, setRules] = useState(() => (host?.forwards ?? []).map(ruleRow));
  const [startup, setStartup] = useState<readonly string[]>(() => host?.startup ?? []);
  const [error, setError] = useState<string | undefined>();
  const [saving, setSaving] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (port(portText) === null) {
      setError("Port must be a whole number");
      return;
    }
    const forwards = rulesToSave(rules);
    if (typeof forwards === "string") {
      setError(forwards);
      return;
    }
    const withOptions = withSession({ ...host?.settings, identity: identity || null }, session);
    if (typeof withOptions === "string") {
      setError(withOptions);
      return;
    }
    setSaving(true);
    try {
      await store.saveHost({
        id: host?.id ?? null,
        label,
        address,
        protocol,
        group: group || null,
        tags: list(tags) ?? [],
        settings: withTriggers(
          withGuard(
            withEdits(withOptions, {
              user,
              port: portText,
              jumps,
              direct,
              identityFiles,
            }),
            guard,
          ),
          highlightSets ? triggers : undefined,
        ),
        forwards,
        // Without the library this window cannot show them; keep them as saved.
        ...(snippets ? { startup: [...startup] } : {}),
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
      <Field label="Protocol">
        <select
          value={protocol}
          onChange={(e) =>
            setProtocol(
              e.target.value === "telnet" ? "telnet" : e.target.value === "mosh" ? "mosh" : "ssh",
            )
          }
          className={FIELD}
        >
          <option value="ssh">SSH</option>
          <option value="mosh">Mosh</option>
          <option value="telnet">Telnet</option>
        </select>
      </Field>
      {protocol === "mosh" && (
        <p className="mb-3 text-xs opacity-70">
          Signs in over SSH with this host's settings, then the terminal runs over Mosh, which keeps
          going across sleep and network changes and shows typing at once. Needs Mosh installed here
          and on the host, UDP ports 60000 to 61000 open to it, and a direct route: no jump host or
          proxy. Files and forwards still use SSH. Not available on Windows.
        </p>
      )}
      {protocol === "telnet" && (
        <p className="mb-3 text-xs text-red-600 dark:text-red-400">
          Telnet sends everything, passwords included, as clear text. It connects on port 23 unless
          this host sets a port; user, jump hosts, keys, the vault identity, and startup snippets
          are not used, and a group that reaches its network through a jump host or a proxy cannot
          hold it.
        </p>
      )}
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
      <IdentityField value={identity} onChange={setIdentity} identities={identities} />
      <SessionFields
        value={session}
        onChange={setSession}
        inherited={host?.forwardsAgent ?? false}
      />
      <GuardFields value={guard} onChange={setGuard} inherited="Inherit (on when tagged prod)" />
      {highlightSets && (
        <HighlightFields value={triggers} sets={highlightSets} onChange={setTriggers} />
      )}
      <ForwardFields rows={rules} onChange={setRules} />
      {snippets && protocol !== "telnet" && (
        <StartupFields value={startup} snippets={snippets} onChange={setStartup} />
      )}
    </Form>
  );
}

function GroupForm({
  store,
  group,
  identities,
  highlightSets,
  titleId,
  onDone,
}: {
  store: HostsStore;
  group: GroupView | undefined;
  identities: readonly IdentityView[];
  highlightSets: readonly TriggerSetView[] | undefined;
  titleId: string;
  onDone: () => void;
}) {
  const [identity, setIdentity] = useState(group?.settings.identity ?? "");
  const [triggers, setTriggers] = useState<readonly string[]>(() => group?.settings.triggers ?? []);
  const { view } = store.getSnapshot();
  const [name, setName] = useState(group?.name ?? "");
  const [parent, setParent] = useState(group?.parent ?? "");
  const [user, setUser] = useState(group?.settings.user ?? "");
  const [portText, setPortText] = useState(group?.settings.port?.toString() ?? "");
  const [jumps, setJumps] = useState(group?.settings.jumps?.join(", ") ?? "");
  const [guard, setGuard] = useState(() => guardFields(group?.settings.guard));
  const [session, setSession] = useState(() => sessionFields(group?.settings));
  const [error, setError] = useState<string | undefined>();
  const [saving, setSaving] = useState(false);
  const excluded = group ? descendants(view.groups, group.id) : new Set<string>();

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (port(portText) === null) {
      setError("Port must be a whole number");
      return;
    }
    const withOptions = withSession({ ...group?.settings, identity: identity || null }, session);
    if (typeof withOptions === "string") {
      setError(withOptions);
      return;
    }
    setSaving(true);
    try {
      await store.saveGroup({
        id: group?.id ?? null,
        name,
        parent: parent || null,
        settings: withTriggers(
          withGuard(withEdits(withOptions, { user, port: portText, jumps }), guard),
          highlightSets ? triggers : undefined,
        ),
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
      <IdentityField value={identity} onChange={setIdentity} identities={identities} />
      <SessionFields value={session} onChange={setSession} />
      <GuardFields value={guard} onChange={setGuard} inherited="Inherit from the parent group" />
      {highlightSets && (
        <HighlightFields value={triggers} sets={highlightSets} onChange={setTriggers} />
      )}
    </Form>
  );
}

/** Agent forwarding and environment variables. */
/**
 * The vault identity to sign in with. While the vault is locked the list is
 * empty, and a chosen identity shows by its id so saving keeps it.
 */
function IdentityField({
  value,
  onChange,
  identities,
}: {
  value: string;
  onChange: (next: string) => void;
  identities: readonly IdentityView[];
}) {
  const known = identities.some((i) => i.id === value);
  return (
    <Field label="Identity from the vault">
      <select value={value} onChange={(e) => onChange(e.target.value)} className={FIELD}>
        <option value="">None (inherit)</option>
        {value && !known && <option value={value}>{`${value} (vault locked)`}</option>}
        {identities.map((identity) => (
          <option key={identity.id} value={identity.id}>
            {identity.user ? `${identity.name} (${identity.user})` : identity.name}
          </option>
        ))}
      </select>
    </Field>
  );
}

function SessionFields({
  value,
  onChange,
  inherited = false,
}: {
  value: SessionFieldsState;
  onChange: (next: SessionFieldsState) => void;
  /** Forwarding is on through a group. */
  inherited?: boolean;
}) {
  const forwards = value.forwardAgent === "on" || (value.forwardAgent === "inherit" && inherited);
  const set = (patch: Partial<SessionFieldsState>) => onChange({ ...value, ...patch });
  return (
    <details className="mb-3 rounded border border-current/15 px-3 py-2">
      <summary className="cursor-pointer select-none">Session</summary>
      <div className="mt-2">
        <Field label="Forward the SSH agent (ssh -A)">
          <select
            value={value.forwardAgent}
            onChange={(e) => set({ forwardAgent: guardChoice(e.target.value) })}
            className={FIELD}
          >
            <option value="inherit">Inherit (off by default)</option>
            <option value="on">On</option>
            <option value="off">Off</option>
          </select>
        </Field>
        {forwards && (
          <p className="mb-3 text-xs text-red-600 dark:text-red-400">
            {value.forwardAgent === "inherit" ? "A group forwards the agent. " : ""}
            Anyone with root on the server can use your agent&apos;s keys while you are connected.
          </p>
        )}
        <Field label="Record commands in the history">
          <select
            value={value.history}
            onChange={(e) => set({ history: guardChoice(e.target.value) })}
            className={FIELD}
          >
            <option value="inherit">Inherit (on by default)</option>
            <option value="on">On</option>
            <option value="off">Off</option>
          </select>
        </Field>
        <Field label="Let the AI assistant read this host's output">
          <select
            value={value.aiOutput}
            onChange={(e) => set({ aiOutput: guardChoice(e.target.value) })}
            className={FIELD}
          >
            <option value="inherit">Inherit (off by default)</option>
            <option value="on">On</option>
            <option value="off">Off</option>
          </select>
        </Field>
        <Field label="Character set">
          <select
            value={value.encoding}
            onChange={(e) => set({ encoding: encodingChoice(e.target.value) })}
            className={FIELD}
          >
            <option value="inherit">Inherit (UTF-8 by default)</option>
            {ENCODINGS.map(([spec, name]) => (
              <option key={spec} value={spec}>
                {name}
              </option>
            ))}
          </select>
        </Field>
        <Field label="tmux to join on every connect (blank inherits, off turns it off)">
          <input
            value={value.tmux === "inherit" ? "" : value.tmux}
            onChange={(e) => set({ tmux: e.target.value === "" ? "inherit" : e.target.value })}
            placeholder="inherit"
            spellCheck={false}
            className={`${FIELD} font-mono`}
          />
        </Field>
        <p className="mb-3 text-xs opacity-60">
          With tmux, a dropped connection leaves your work running on the host, and the next connect
          picks it up where it was.
        </p>
        <Field label="Environment variables (one per line, as LANG=C.UTF-8)">
          <textarea
            value={value.env}
            onChange={(e) => set({ env: e.target.value })}
            rows={2}
            placeholder="LC_ALL=C.UTF-8"
            spellCheck={false}
            className={`${FIELD} font-mono`}
          />
        </Field>
        <p className="text-xs opacity-60">
          The server keeps only the names its AcceptEnv allows. Values are saved in the host list as
          plain text: keep secrets out of them.
        </p>
      </div>
    </details>
  );
}

/**
 * The prod guard's settings. Patterns are command globs, one per line, matched
 * against the command as it would run, after quotes and `sudo` are removed.
 */
function GuardFields({
  value,
  onChange,
  inherited,
}: {
  value: GuardFieldsState;
  onChange: (next: GuardFieldsState) => void;
  inherited: string;
}) {
  const set = (patch: Partial<GuardFieldsState>) => onChange({ ...value, ...patch });
  return (
    <details
      className="mb-3 rounded border border-current/15 px-3 py-2"
      open={value.enabled === "on"}
    >
      <summary className="cursor-pointer select-none">Prod guard</summary>
      <div className="mt-2">
        <Field label="Check commands before they run">
          <select
            value={value.enabled}
            onChange={(e) => set({ enabled: guardChoice(e.target.value) })}
            className={FIELD}
          >
            <option value="inherit">{inherited}</option>
            <option value="on">On</option>
            <option value="off">Off</option>
          </select>
        </Field>
        <Field label="Always ask before (one pattern per line, * matches anything)">
          <textarea
            value={value.deny}
            onChange={(e) => set({ deny: e.target.value })}
            rows={2}
            placeholder="systemctl stop *"
            spellCheck={false}
            className={`${FIELD} font-mono`}
          />
        </Field>
        <Field label="Never ask before">
          <textarea
            value={value.allow}
            onChange={(e) => set({ allow: e.target.value })}
            rows={2}
            placeholder="rm -rf /tmp/build/*"
            spellCheck={false}
            className={`${FIELD} font-mono`}
          />
        </Field>
        <p className="text-xs opacity-60">
          Patterns from the host and all its groups apply. A deny pattern always asks, even when an
          allow pattern matches.
        </p>
      </div>
    </details>
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
      {/* Sticks to the bottom of a dialog taller than the window, so Save stays in reach. */}
      <div className="sticky -bottom-4 -mx-4 -mb-4 flex items-center gap-2 border-t border-current/10 bg-background px-4 py-3">
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

/** `settings` with the highlight sets chosen; unchanged when the choice was not shown. */
export function withTriggers(
  settings: SettingsSpec,
  triggers: readonly string[] | undefined,
): SettingsSpec {
  return triggers === undefined ? settings : { ...settings, triggers: [...triggers] };
}

/** Most highlight sets one host or group names, as the backend allows. */
const MAX_SETS = 8;

/**
 * The highlight sets a host or group names, in precedence order. They add
 * up with its groups', its own first.
 */
export function HighlightFields({
  value,
  sets,
  onChange,
}: {
  value: readonly string[];
  sets: readonly TriggerSetView[];
  onChange: (value: readonly string[]) => void;
}) {
  const selectId = useId();
  const byId = new Map(sets.map((s) => [s.id, s]));
  const left = sets.filter((s) => !value.includes(s.id));
  return (
    <details className="mb-3 rounded border border-current/15 px-3 py-2" open={value.length > 0}>
      <summary className="cursor-pointer select-none">Highlight sets</summary>
      <div className="mt-2 flex flex-col gap-1">
        {value.length === 0 && (
          <p className="opacity-60">
            Colour what output matches. Sets named here add to those of the groups above.
          </p>
        )}
        <ol aria-label="Highlight sets" className="flex flex-col gap-1">
          {value.map((id, i) => {
            const name = byId.get(id)?.name ?? `${id} (deleted)`;
            return (
              <li key={id} className="flex items-center gap-2">
                <span className="w-5 text-right opacity-60">{i + 1}.</span>
                <span className="min-w-0 flex-1 truncate">{name}</span>
                <button
                  type="button"
                  className="rounded px-2 hover:bg-current/10 disabled:opacity-40"
                  aria-label={`Put ${name} first`}
                  disabled={i === 0}
                  onClick={() => onChange([id, ...value.filter((other) => other !== id)])}
                >
                  ↑
                </button>
                <button
                  type="button"
                  className="rounded px-2 hover:bg-current/10"
                  aria-label={`Remove highlight set ${name}`}
                  onClick={() => onChange(value.filter((other) => other !== id))}
                >
                  ×
                </button>
              </li>
            );
          })}
        </ol>
        {left.length > 0 && value.length < MAX_SETS && (
          <label htmlFor={selectId} className="flex items-center gap-2">
            Add
            <select
              id={selectId}
              aria-label="Add a highlight set"
              value=""
              onChange={(e) => {
                if (e.target.value) onChange([...value, e.target.value]);
              }}
              className={FIELD}
            >
              <option value="">Choose a set</option>
              {left.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
    </details>
  );
}

/** Most startup snippets a host runs, as the backend allows. */
const MAX_STARTUP = 8;

/** The snippets typed into each new shell on the host, in order. */
export function StartupFields({
  value,
  snippets,
  onChange,
}: {
  value: readonly string[];
  snippets: readonly SnippetView[];
  onChange: (value: readonly string[]) => void;
}) {
  const selectId = useId();
  const byId = new Map(snippets.map((s) => [s.id, s]));
  const left = snippets.filter((s) => !value.includes(s.id));
  return (
    <fieldset className="flex flex-col gap-1 rounded border border-current/15 p-2">
      <legend className="px-1">Startup snippets</legend>
      {value.length === 0 && (
        <p className="opacity-60">Each new shell on this host can start by running snippets.</p>
      )}
      <ol aria-label="Startup snippets" className="flex flex-col gap-1">
        {value.map((id, i) => {
          const name = byId.get(id)?.name ?? `${id} (deleted)`;
          return (
            <li key={id} className="flex items-center gap-2">
              <span className="w-5 text-right opacity-60">{i + 1}.</span>
              <span className="min-w-0 flex-1 truncate">{name}</span>
              <button
                type="button"
                className="rounded px-2 hover:bg-current/10 disabled:opacity-40"
                aria-label={`Run ${name} earlier`}
                disabled={i === 0}
                onClick={() => {
                  const next = [...value];
                  [next[i - 1], next[i]] = [id, value[i - 1] ?? id];
                  onChange(next);
                }}
              >
                ↑
              </button>
              <button
                type="button"
                className="rounded px-2 hover:bg-current/10"
                aria-label={`Remove ${name}`}
                onClick={() => onChange(value.filter((other) => other !== id))}
              >
                ×
              </button>
            </li>
          );
        })}
      </ol>
      {left.length > 0 && value.length < MAX_STARTUP && (
        <label htmlFor={selectId} className="flex items-center gap-2">
          Add
          <select
            id={selectId}
            value=""
            onChange={(e) => {
              if (e.target.value) onChange([...value, e.target.value]);
            }}
            className={FIELD}
          >
            <option value="">Choose a snippet</option>
            {left.map((s) => (
              <option key={s.id} value={s.id}>
                {s.folder ? `${s.folder} / ${s.name}` : s.name}
              </option>
            ))}
          </select>
        </label>
      )}
    </fieldset>
  );
}
