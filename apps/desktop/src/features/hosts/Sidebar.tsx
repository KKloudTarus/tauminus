// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useDeferredValue, useMemo, useState, useSyncExternalStore } from "react";

import { Icon } from "@/app/icons.tsx";
import { formatEndpoint } from "@/features/ssh/destination.ts";
import type {
  GroupView,
  HostView,
  IdentityView,
  SnippetView,
  TriggerSetView,
} from "@/ipc/index.ts";

import { type Editing, EditorDialog } from "./HostEditor.tsx";
import type { HostsStore } from "./hosts-store.ts";
import { searchHosts } from "./search.ts";

interface SidebarProps {
  store: HostsStore;
  onConnect: (host: HostView) => void;
  onOpenVault?: () => void;
  /** Opens the host's files; without it there is no files button. */
  onFiles?: (host: HostView) => void;
  /** Identities to pick from in the editors; empty while the vault is locked. */
  identities?: readonly IdentityView[];
  /** Snippets a host can run when it connects; without them there is no choice. */
  snippets?: readonly SnippetView[] | undefined;
  /** Highlight sets a host or group can name; without them there is no choice. */
  highlightSets?: readonly TriggerSetView[] | undefined;
  /** Layout classes for the root: docked beside the terminal or a drawer over it. */
  className?: string;
}

interface Tree {
  groups: Map<string, GroupView[]>;
  hosts: Map<string, HostView[]>;
}

const ROOT = "";
const NO_IDENTITIES: readonly IdentityView[] = [];
/** Rows rendered for a search; more would only slow typing down. */
const MAX_RESULTS = 200;

function add<T>(map: Map<string, T[]>, key: string, item: T): void {
  const list = map.get(key);
  if (list) list.push(item);
  else map.set(key, [item]);
}

/** Children of each group, sorted by name. O((G + H) log(G + H)). */
function buildTree(groups: readonly GroupView[], hosts: readonly HostView[]): Tree {
  const tree: Tree = { groups: new Map(), hosts: new Map() };
  const known = new Set(groups.map((g) => g.id));
  for (const group of groups) {
    add(tree.groups, group.parent && known.has(group.parent) ? group.parent : ROOT, group);
  }
  for (const host of hosts) {
    add(tree.hosts, host.group && known.has(host.group) ? host.group : ROOT, host);
  }
  for (const list of tree.groups.values()) list.sort((a, b) => a.name.localeCompare(b.name));
  for (const list of tree.hosts.values()) list.sort((a, b) => a.label.localeCompare(b.label));
  return tree;
}

/** Saved hosts: search, the group tree, and the editors. */
export function Sidebar({
  store,
  onConnect,
  onOpenVault,
  onFiles,
  identities = NO_IDENTITIES,
  snippets,
  highlightSets,
  className = "w-64 border-r",
}: SidebarProps) {
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<Editing | undefined>();
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const { groups, hosts } = state.view;
  const tree = useMemo(() => buildTree(groups, hosts), [groups, hosts]);
  const counts = useMemo(() => countHosts(tree), [tree]);
  // Typing stays responsive; results catch up a frame later on big lists.
  const deferredQuery = useDeferredValue(query);
  const results = useMemo(
    () => (deferredQuery.trim() ? searchHosts(hosts, deferredQuery, store.groupName) : []),
    [hosts, deferredQuery, store],
  );

  const toggle = (id: string) => {
    const next = new Set(collapsed);
    if (!next.delete(id)) next.add(id);
    setCollapsed(next);
  };

  const row = (host: HostView, depth: number, groupLabel?: string) => (
    <HostRow
      key={host.id}
      host={host}
      depth={depth}
      groupLabel={groupLabel}
      onConnect={onConnect}
      onEdit={() => setEditing({ kind: "host", host })}
      {...(onFiles && host.protocol !== "telnet" ? { onFiles: () => onFiles(host) } : {})}
    />
  );

  const branch = (parent: string, depth: number): React.ReactNode[] => [
    ...(tree.groups.get(parent) ?? []).map((group) => {
      const open = !collapsed.has(group.id);
      return (
        <li key={group.id} className="mt-1 first:mt-0">
          <div className="group flex items-center" style={{ paddingLeft: `${depth * 12}px` }}>
            <button
              type="button"
              aria-expanded={open}
              // The label is set in capitals; screen readers get the name as written.
              aria-label={group.name}
              onClick={() => toggle(group.id)}
              className="flex h-7 min-w-0 flex-1 items-center gap-1.5 rounded-md px-1.5 text-left text-[11px] font-semibold tracking-[0.06em] text-faint uppercase hover:text-muted"
            >
              <span
                aria-hidden="true"
                className={`mask-icon i-chevron size-3 transition-transform ${open ? "rotate-90" : ""}`}
              />
              <span className="truncate">{group.name}</span>
              <span aria-hidden="true" className="font-medium tracking-normal opacity-70">
                {counts.get(group.id) ?? 0}
              </span>
            </button>
            <span className="pr-1">
              <RowAction
                label={`Edit group ${group.name}`}
                onClick={() => setEditing({ kind: "group", group })}
              />
            </span>
          </div>
          {open && <ul className="flex flex-col gap-px">{branch(group.id, depth + 1)}</ul>}
        </li>
      );
    }),
    ...(tree.hosts.get(parent) ?? []).map((host) => row(host, depth)),
  ];

  const empty = state.status === "ready" && hosts.length === 0 && groups.length === 0;

  return (
    <aside
      aria-label="Hosts"
      className={`flex shrink-0 flex-col border-line bg-surface-1 text-[13px] ${className}`}
    >
      <div className="flex h-11 items-center gap-0.5 pr-2 pl-3.5">
        <h2 className="flex-1 text-[13px] font-semibold tracking-[-0.005em]">Hosts</h2>
        {onOpenVault && (
          <button
            type="button"
            onClick={onOpenVault}
            aria-label="Vault"
            title="Vault"
            className="icon-btn"
          >
            <Icon name="key" />
          </button>
        )}
        <button
          type="button"
          onClick={() => setEditing({ kind: "group", group: undefined })}
          aria-label="New group"
          title="New group"
          className="icon-btn"
        >
          <Icon name="folderPlus" />
        </button>
        <button
          type="button"
          onClick={() => setEditing({ kind: "host", host: undefined })}
          aria-label="New host"
          title="New host"
          className="icon-btn"
        >
          <Icon name="plus" />
        </button>
      </div>
      <div className="relative px-2.5 pb-2">
        <Icon
          name="search"
          className="pointer-events-none absolute top-1/2 left-5 size-3.5 -translate-y-[calc(50%+4px)] text-faint"
        />
        <input
          type="search"
          aria-label="Search hosts"
          placeholder="Search name, address, tag"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            const first = results[0];
            if (e.key === "Enter" && first) {
              // The connect can open a prompt that takes focus before this
              // key's default action runs; without this, Enter would land on
              // the prompt's focused button.
              e.preventDefault();
              onConnect(first.host);
            }
            // Escape clears a query; with none it is left to whatever holds the list.
            if (e.key === "Escape" && query) {
              e.preventDefault();
              setQuery("");
            }
          }}
          className="field !min-h-8 bg-background pl-8 text-[13px]"
        />
      </div>
      {state.notice && (
        <output className="mx-2.5 mb-2 flex items-start gap-2 rounded-lg border border-line bg-surface-2 py-2 pr-1 pl-2.5 text-xs">
          <span className="flex-1 pt-0.5">{state.notice}</span>
          <button
            type="button"
            onClick={() => store.dismissNotice()}
            aria-label="Dismiss"
            className="icon-btn size-5 pointer-coarse:size-10"
          >
            <Icon name="x" className="size-3.5" />
          </button>
        </output>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-3">
        {state.status === "loading" && <p className="px-2 text-faint">Loading…</p>}
        {state.status === "failed" && (
          <p role="alert" className="px-2 text-error">
            Saved hosts are unavailable: {state.error}. The file is left as it is.
          </p>
        )}
        {empty && (
          <FirstRun store={store} onAdd={() => setEditing({ kind: "host", host: undefined })} />
        )}
        {query.trim() ? (
          results.length > 0 ? (
            <>
              <ul aria-label="Search results" className="flex flex-col gap-px">
                {results.slice(0, MAX_RESULTS).map((m) => row(m.host, 0, store.groupName(m.host)))}
              </ul>
              {results.length > MAX_RESULTS && (
                <p className="px-2 pt-2 text-xs text-faint">
                  Showing the best {MAX_RESULTS} of {results.length}. Type more to narrow it down.
                </p>
              )}
            </>
          ) : (
            <p className="px-2 py-6 text-center text-faint">No host matches.</p>
          )
        ) : (
          <ul aria-label="Saved hosts" className="flex flex-col gap-px">
            {branch(ROOT, 0)}
          </ul>
        )}
      </div>
      {editing && (
        <EditorDialog
          store={store}
          editing={editing}
          identities={identities}
          {...(snippets ? { snippets } : {})}
          {...(highlightSets ? { highlightSets } : {})}
          onClose={() => setEditing(undefined)}
        />
      )}
    </aside>
  );
}

function HostRow({
  host,
  depth,
  groupLabel,
  onConnect,
  onEdit,
  onFiles,
}: {
  host: HostView;
  depth: number;
  groupLabel: string | undefined;
  onConnect: (host: HostView) => void;
  onEdit: () => void;
  onFiles?: () => void;
}) {
  const where = host.target ? formatEndpoint(host.target.destination) : host.address;
  // Lean on purpose: this row repeats for every saved host. Off-screen rows
  // skip layout and paint (content-visibility), which keeps 2000 hosts cheap.
  return (
    <li
      className="group flex items-center rounded-lg pr-1 [contain-intrinsic-size:auto_44px] [content-visibility:auto] focus-within:bg-surface-2 hover:bg-surface-2"
      style={{ marginLeft: `${depth * 12}px` }}
    >
      <button
        type="button"
        onClick={() => onConnect(host)}
        title={host.problem ?? `Connect to ${where}`}
        className="flex min-w-0 flex-1 items-center gap-2.5 rounded-lg py-1.5 pr-1 pl-1.5 text-left focus-visible:outline-offset-[-2px]"
      >
        <Avatar host={host} />
        <span className="grid min-w-0 flex-1">
          <span className="flex items-center gap-1.5">
            {host.problem && (
              <span aria-label="Cannot connect" className="font-semibold text-warn">
                !
              </span>
            )}
            <span className="truncate font-medium">{host.label}</span>
            {host.guarded && (
              <span aria-hidden="true" className="chip chip-prod !h-4 shrink-0 !px-1 !text-[9.5px]">
                PROD
              </span>
            )}
          </span>
          <span className="truncate text-xs text-faint">
            {where}
            {groupLabel ? ` · ${groupLabel}` : ""}
            {host.tags.length > 0 ? ` · ${host.tags.join(" ")}` : ""}
          </span>
        </span>
      </button>
      {onFiles && <RowAction label={`Files on ${host.label}`} icon="files" onClick={onFiles} />}
      <RowAction label={`Edit ${host.label}`} onClick={onEdit} />
    </li>
  );
}

/** Hues for host monograms; a host keeps its color across launches. */
const HUES = [250, 200, 160, 290, 30, 330, 100];

/** A two-letter monogram on a color picked by hashing the label. O(label). */
function Avatar({ host }: { host: HostView }) {
  let hash = 0;
  for (let i = 0; i < host.label.length; i++) hash = (hash * 31 + host.label.charCodeAt(i)) | 0;
  const hue = HUES[Math.abs(hash) % HUES.length] ?? 250;
  const letters = host.label
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0])
    .join("")
    .toUpperCase();
  return (
    <span
      aria-hidden="true"
      className="relative flex size-7 shrink-0 items-center justify-center rounded-[7px] text-[10.5px] font-semibold tracking-wide"
      style={
        host.guarded
          ? { background: "var(--prod-soft)", color: "var(--prod)" }
          : {
              background: `oklch(0.62 0.1 ${hue} / 0.16)`,
              color: `oklch(var(--avatar-l) 0.13 ${hue})`,
            }
      }
    >
      {letters || "?"}
      {host.protocol !== "ssh" && (
        <span className="absolute -right-1 -bottom-1 rounded-[4px] border border-surface-1 bg-surface-3 px-0.5 text-[8px] leading-3 font-bold text-muted uppercase">
          {host.protocol === "telnet" ? "TEL" : host.protocol === "mosh" ? "MSH" : host.protocol}
        </span>
      )}
    </span>
  );
}

/** Hosts under each group, subgroups included. One post-order walk: O(G + H). */
function countHosts(tree: Tree): Map<string, number> {
  const counts = new Map<string, number>();
  const visit = (group: string): number => {
    let count = tree.hosts.get(group)?.length ?? 0;
    for (const child of tree.groups.get(group) ?? []) count += visit(child.id);
    counts.set(group, count);
    return count;
  };
  visit(ROOT);
  return counts;
}

/** A row's action: shown on hover or focus, and always on touch screens. */
function RowAction({
  label,
  onClick,
  icon = "pencil",
}: {
  label: string;
  onClick: () => void;
  icon?: "pencil" | "files";
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className="icon-btn size-6 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 hover:bg-surface-3 pointer-coarse:size-10 pointer-coarse:opacity-100"
    >
      <span aria-hidden="true" className={`mask-icon i-${icon}`} />
    </button>
  );
}

function FirstRun({ store, onAdd }: { store: HostsStore; onAdd: () => void }) {
  const { configAliases } = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [error, setError] = useState<string | undefined>();
  return (
    <div className="m-1 rounded-xl border border-dashed border-line-strong p-4">
      <div className="mb-3 flex size-9 items-center justify-center rounded-lg bg-accent-soft text-accent">
        <Icon name="server" className="size-[18px]" />
      </div>
      <p className="mb-1 font-semibold">No saved hosts yet.</p>
      <p className="mb-3 text-xs text-muted">Save a host once, then connect with one click.</p>
      {configAliases > 0 && (
        <button
          type="button"
          onClick={() => {
            store.importConfig().catch((e: unknown) => setError(String(e)));
          }}
          className="btn btn-primary mb-2 h-auto w-full py-1.5 text-center whitespace-normal"
        >
          Import {configAliases} {configAliases === 1 ? "host" : "hosts"} from ~/.ssh/config
        </button>
      )}
      {configAliases > 0 && (
        <p className="mb-3 text-xs text-faint">
          Each host is read with ssh -G, as ssh itself would: Match exec lines in your config run
          too.
        </p>
      )}

      <button type="button" onClick={onAdd} className="btn block w-full">
        Add a host
      </button>
      {error && (
        <p role="alert" className="mt-2 text-error">
          {error}
        </p>
      )}
    </div>
  );
}
