// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useDeferredValue, useMemo, useState, useSyncExternalStore } from "react";

import { formatEndpoint } from "@/features/ssh/destination.ts";
import type { GroupView, HostView, IdentityView, SnippetView } from "@/ipc/index.ts";

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
}: SidebarProps) {
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<Editing | undefined>();
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const { groups, hosts } = state.view;
  const tree = useMemo(() => buildTree(groups, hosts), [groups, hosts]);
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
      {...(onFiles ? { onFiles: () => onFiles(host) } : {})}
    />
  );

  const branch = (parent: string, depth: number): React.ReactNode[] => [
    ...(tree.groups.get(parent) ?? []).map((group) => {
      const open = !collapsed.has(group.id);
      return (
        <li key={group.id}>
          <div className="group flex items-center" style={{ paddingLeft: `${depth * 12}px` }}>
            <button
              type="button"
              aria-expanded={open}
              onClick={() => toggle(group.id)}
              className="flex min-w-0 flex-1 items-center gap-1 rounded px-2 py-1 text-left font-medium hover:bg-current/10"
            >
              <span aria-hidden="true" className="w-3 text-xs opacity-60">
                {open ? "▾" : "▸"}
              </span>
              <span className="truncate">{group.name}</span>
            </button>
            <IconButton
              label={`Edit group ${group.name}`}
              onClick={() => setEditing({ kind: "group", group })}
            />
          </div>
          {open && <ul>{branch(group.id, depth + 1)}</ul>}
        </li>
      );
    }),
    ...(tree.hosts.get(parent) ?? []).map((host) => row(host, depth)),
  ];

  const empty = state.status === "ready" && hosts.length === 0 && groups.length === 0;

  return (
    <aside
      aria-label="Hosts"
      className="flex w-64 shrink-0 flex-col border-r border-current/10 text-sm"
    >
      <div className="flex items-center gap-1 px-2 pt-2">
        <h2 className="flex-1 px-1 font-semibold">Hosts</h2>
        {onOpenVault && (
          <button
            type="button"
            onClick={onOpenVault}
            className="rounded px-1.5 py-1 text-xs whitespace-nowrap hover:bg-current/10"
          >
            Vault
          </button>
        )}
        <button
          type="button"
          onClick={() => setEditing({ kind: "group", group: undefined })}
          className="rounded px-1.5 py-1 text-xs whitespace-nowrap hover:bg-current/10"
        >
          New group
        </button>
        <button
          type="button"
          onClick={() => setEditing({ kind: "host", host: undefined })}
          className="rounded px-1.5 py-1 text-xs whitespace-nowrap hover:bg-current/10"
        >
          New host
        </button>
      </div>
      <div className="p-2">
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
            if (e.key === "Escape") {
              e.preventDefault();
              setQuery("");
            }
          }}
          className="w-full rounded border border-current/20 bg-transparent px-2 py-1.5 outline-none focus:border-current/50"
        />
      </div>
      {state.notice && (
        <output className="mx-2 mb-2 flex gap-2 rounded border border-current/15 p-2 text-xs">
          <span className="flex-1">{state.notice}</span>
          <button
            type="button"
            onClick={() => store.dismissNotice()}
            aria-label="Dismiss"
            className="opacity-60"
          >
            ×
          </button>
        </output>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto px-1 pb-2">
        {state.status === "loading" && <p className="px-2 opacity-60">Loading…</p>}
        {state.status === "failed" && (
          <p role="alert" className="px-2 text-red-600 dark:text-red-400">
            Saved hosts are unavailable: {state.error}. The file is left as it is.
          </p>
        )}
        {empty && (
          <FirstRun store={store} onAdd={() => setEditing({ kind: "host", host: undefined })} />
        )}
        {query.trim() ? (
          results.length > 0 ? (
            <>
              <ul aria-label="Search results">
                {results.slice(0, MAX_RESULTS).map((m) => row(m.host, 0, store.groupName(m.host)))}
              </ul>
              {results.length > MAX_RESULTS && (
                <p className="px-2 text-xs opacity-60">
                  Showing the best {MAX_RESULTS} of {results.length}. Type more to narrow it down.
                </p>
              )}
            </>
          ) : (
            <p className="px-2 opacity-60">No host matches.</p>
          )
        ) : (
          <ul aria-label="Saved hosts">{branch(ROOT, 0)}</ul>
        )}
      </div>
      {editing && (
        <EditorDialog
          store={store}
          editing={editing}
          identities={identities}
          {...(snippets ? { snippets } : {})}
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
  return (
    <li>
      <div className="group flex items-center" style={{ paddingLeft: `${depth * 12 + 16}px` }}>
        <button
          type="button"
          onClick={() => onConnect(host)}
          title={host.problem ?? `Connect to ${where}`}
          className="min-w-0 flex-1 rounded px-2 py-1 text-left hover:bg-current/10"
        >
          <span className="block truncate">
            {host.problem && (
              <span aria-label="Cannot connect" className="mr-1 text-amber-600">
                !
              </span>
            )}
            {host.label}
          </span>
          <span className="block truncate text-xs opacity-60">
            {where}
            {groupLabel ? ` · ${groupLabel}` : ""}
            {host.tags.length > 0 ? ` · ${host.tags.join(" ")}` : ""}
          </span>
        </button>
        {onFiles && <IconButton label={`Files on ${host.label}`} glyph="▤" onClick={onFiles} />}
        <IconButton label={`Edit ${host.label}`} onClick={onEdit} />
      </div>
    </li>
  );
}

function IconButton({
  label,
  onClick,
  glyph = "✎",
}: {
  label: string;
  onClick: () => void;
  glyph?: string;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className="rounded px-2 py-1 opacity-0 group-hover:opacity-60 hover:bg-current/10 focus-visible:opacity-100"
    >
      {glyph}
    </button>
  );
}

function FirstRun({ store, onAdd }: { store: HostsStore; onAdd: () => void }) {
  const { configAliases } = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [error, setError] = useState<string | undefined>();
  return (
    <div className="m-2 rounded border border-current/15 p-3">
      <p className="mb-2">No saved hosts yet.</p>
      {configAliases > 0 && (
        <button
          type="button"
          onClick={() => {
            store.importConfig().catch((e: unknown) => setError(String(e)));
          }}
          className="mb-2 block w-full rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90"
        >
          Import {configAliases} {configAliases === 1 ? "host" : "hosts"} from ~/.ssh/config
        </button>
      )}
      {configAliases > 0 && (
        <p className="mb-2 text-xs opacity-60">
          Each host is read with ssh -G, as ssh itself would: Match exec lines in your config run
          too.
        </p>
      )}

      <button
        type="button"
        onClick={onAdd}
        className="block w-full rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
      >
        Add a host
      </button>
      {error && (
        <p role="alert" className="mt-2 text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
    </div>
  );
}
