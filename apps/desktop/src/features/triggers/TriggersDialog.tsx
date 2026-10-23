// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FormEvent,
  memo,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

import type {
  ActionSpec,
  RuleSpec,
  StyleSpec,
  TriggerSetView,
  TriggersClient,
} from "@/ipc/index.ts";

import type { TriggersStore } from "./triggers-store.ts";

const BUTTON = "rounded px-2 py-0.5 hover:bg-current/10 disabled:opacity-40";
const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1 outline-none focus:border-current/50";

/** The terminal palette's colours, by the index rules store. */
export const PALETTE_NAMES = [
  "Black",
  "Red",
  "Green",
  "Yellow",
  "Blue",
  "Magenta",
  "Cyan",
  "White",
  "Bright black",
  "Bright red",
  "Bright green",
  "Bright yellow",
  "Bright blue",
  "Bright magenta",
  "Bright cyan",
  "Bright white",
] as const;

type Editing = { kind: "new" } | { kind: "edit"; set: TriggerSetView };

/** Highlight sets: add, edit, delete, and choose those every session gets. */
export function TriggersDialog({ store, onClose }: { store: TriggersStore; onClose: () => void }) {
  const titleId = useId();
  const state = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dialog = useRef<HTMLDialogElement>(null);
  const [editing, setEditing] = useState<Editing | undefined>();
  useEffect(() => dialog.current?.focus(), []);
  const edit = useCallback(
    (id: string) => {
      const set = store.getSnapshot().sets.find((s) => s.id === id);
      if (set) setEditing({ kind: "edit", set });
    },
    [store],
  );
  const remove = useCallback((id: string) => void store.remove(id), [store]);
  const setDefault = useCallback(
    (id: string, on: boolean) => void store.setDefault(id, on),
    [store],
  );

  return (
    <div className="fixed inset-0 z-20 flex items-start justify-center bg-black/30 pt-[8vh]">
      <dialog
        ref={dialog}
        open
        aria-labelledby={titleId}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            if (editing) setEditing(undefined);
            else onClose();
          }
        }}
        className="static m-0 max-h-[80vh] w-[min(760px,94vw)] overflow-y-auto rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl outline-none"
      >
        <div className="mb-3 flex items-center justify-between gap-2">
          <h2 id={titleId} className="text-base font-medium">
            Highlight sets
          </h2>
          <div className="flex gap-1">
            <button type="button" className={BUTTON} onClick={() => setEditing({ kind: "new" })}>
              New set
            </button>
            <button type="button" className={BUTTON} onClick={onClose}>
              Close
            </button>
          </div>
        </div>
        {state.error && (
          <p role="alert" className="mb-2 text-red-600 dark:text-red-400">
            {state.error}
          </p>
        )}
        {editing && (
          <SetEditor
            key={editing.kind === "edit" ? editing.set.id : "new"}
            set={editing.kind === "edit" ? editing.set : undefined}
            client={store.client}
            onSave={async (input) => {
              if (await store.save(input)) setEditing(undefined);
            }}
            onCancel={() => setEditing(undefined)}
          />
        )}
        {state.loaded && state.sets.length === 0 && !editing && (
          <p className="opacity-70">
            No highlight sets yet. A set colours what its patterns match in a terminal; attach it to
            hosts and groups in their settings, or apply it to every session here.
          </p>
        )}
        <ul aria-label="Highlight sets" className="flex flex-col gap-1">
          {state.sets.map((set) => (
            <SetRow
              key={set.id}
              id={set.id}
              name={set.name}
              rules={set.rules.length}
              isDefault={set.rank !== null && set.rank !== undefined}
              onEdit={edit}
              onDelete={remove}
              onDefault={setDefault}
            />
          ))}
        </ul>
      </dialog>
    </div>
  );
}

const SetRow = memo(function SetRow({
  id,
  name,
  rules,
  isDefault,
  onEdit,
  onDelete,
  onDefault,
}: {
  id: string;
  name: string;
  rules: number;
  isDefault: boolean;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
  onDefault: (id: string, on: boolean) => void;
}) {
  return (
    <li className="flex items-center gap-2">
      <span className="min-w-0 flex-1 truncate">{name}</span>
      <span className="text-xs opacity-60">
        {rules} {rules === 1 ? "rule" : "rules"}
      </span>
      <label className="flex items-center gap-1 text-xs">
        <input
          type="checkbox"
          checked={isDefault}
          onChange={(e) => onDefault(id, e.target.checked)}
          aria-label={`Apply ${name} to every session`}
        />
        Every session
      </label>
      <button
        type="button"
        className={BUTTON}
        aria-label={`Edit ${name}`}
        onClick={() => onEdit(id)}
      >
        Edit
      </button>
      <button
        type="button"
        className={BUTTON}
        aria-label={`Delete ${name}`}
        onClick={() => onDelete(id)}
      >
        Delete
      </button>
    </li>
  );
});

const NEW_STYLE: StyleSpec = { fg: "1", bg: null, line: false };

const NEW_RULE: RuleSpec = {
  name: "",
  pattern: "",
  enabled: true,
  action: { kind: "highlight", style: NEW_STYLE },
};

/** What each action is called in the editor. */
const ACTIONS: { kind: ActionSpec["kind"]; label: string }[] = [
  { kind: "highlight", label: "Colour the match" },
  { kind: "notify", label: "Notify me" },
  { kind: "capture", label: "Keep the line" },
  { kind: "send", label: "Send text" },
];

/** A fresh action of `kind`, keeping what carries over from `from`. */
function actionOf(kind: ActionSpec["kind"], from: ActionSpec): ActionSpec {
  switch (kind) {
    case "highlight":
      return { kind, style: from.kind === "highlight" ? from.style : NEW_STYLE };
    case "send":
      return from.kind === "send" ? from : { kind, text: "", enter: true };
    case "notify":
    case "capture":
    default:
      return { kind: kind === "capture" ? "capture" : "notify" };
  }
}

function SetEditor({
  set,
  client,
  onSave,
  onCancel,
}: {
  set: TriggerSetView | undefined;
  client: TriggersClient;
  onSave: (input: { id?: string; name: string; rules: RuleSpec[] }) => Promise<void>;
  onCancel: () => void;
}) {
  const [name, setName] = useState(set?.name ?? "");
  const [rules, setRules] = useState<RuleSpec[]>(set?.rules ?? [NEW_RULE]);
  const nameId = useId();
  const change = (at: number, rule: RuleSpec) =>
    setRules((all) => all.map((r, i) => (i === at ? rule : r)));
  const submit = (event: FormEvent) => {
    event.preventDefault();
    void onSave({
      ...(set ? { id: set.id } : {}),
      name,
      // An unnamed rule is called by its pattern.
      rules: rules.map((r) => ({
        ...r,
        name: r.name.trim() || Array.from(r.pattern).slice(0, 64).join(""),
      })),
    });
  };
  return (
    <form
      aria-label={set ? `Edit ${set.name}` : "New highlight set"}
      onSubmit={submit}
      className="mb-4 flex flex-col gap-2 rounded border border-current/15 p-3"
    >
      <label htmlFor={nameId} className="text-xs opacity-70">
        Name
      </label>
      <input
        id={nameId}
        value={name}
        onChange={(e) => setName(e.target.value)}
        className={FIELD}
        placeholder="Errors and warnings"
      />
      <p className="text-xs opacity-70">
        Patterns are regular expressions; <code>(?i)</code> ignores case. An earlier rule wins where
        two colour the same text. Other actions run once for each finished line of output, not in
        full-screen programs; sent text goes through the prod guard, and is not sent at a password
        prompt.
      </p>
      <ol aria-label="Rules" className="flex flex-col gap-2">
        {rules.map((rule, at) => (
          <RuleEditor
            // oxlint-disable-next-line react/no-array-index-key -- rules have no ids; order is the identity
            key={at}
            index={at}
            rule={rule}
            client={client}
            onChange={(next) => change(at, next)}
            onRemove={() => setRules((all) => all.filter((_, i) => i !== at))}
          />
        ))}
      </ol>
      <div className="flex justify-between gap-2">
        <button
          type="button"
          className={BUTTON}
          onClick={() => setRules((all) => [...all, NEW_RULE])}
          disabled={rules.length >= 100}
        >
          Add rule
        </button>
        <div className="flex gap-1">
          <button type="button" className={BUTTON} onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className={BUTTON} disabled={name.trim() === ""}>
            Save
          </button>
        </div>
      </div>
    </form>
  );
}

function RuleEditor({
  index,
  rule,
  client,
  onChange,
  onRemove,
}: {
  index: number;
  rule: RuleSpec;
  client: TriggersClient;
  onChange: (rule: RuleSpec) => void;
  onRemove: () => void;
}) {
  const [checked, setChecked] = useState<{ pattern: string; problem: string | null }>();
  const problem = checked?.pattern === rule.pattern ? checked.problem : null;
  useEffect(() => {
    if (rule.pattern === "") return undefined;
    let current = true;
    const timer = setTimeout(() => {
      client
        .check(rule.pattern)
        .then((found) => {
          if (current) setChecked({ pattern: rule.pattern, problem: found });
        })
        .catch(() => {});
    }, 200);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [client, rule.pattern]);
  const label = `Rule ${index + 1}`;
  const { action } = rule;
  const style = (change: Partial<StyleSpec>) => {
    if (action.kind === "highlight") {
      onChange({ ...rule, action: { kind: "highlight", style: { ...action.style, ...change } } });
    }
  };
  return (
    <li aria-label={label} className="flex flex-col gap-1 rounded bg-current/5 p-2">
      <div className="flex items-center gap-2">
        <input
          aria-label={`${label} pattern`}
          value={rule.pattern}
          onChange={(e) => onChange({ ...rule, pattern: e.target.value })}
          className={`${FIELD} flex-1 font-mono`}
          placeholder="ERROR|FATAL"
          spellCheck={false}
        />
        <input
          aria-label={`${label} name`}
          value={rule.name}
          onChange={(e) => onChange({ ...rule, name: e.target.value })}
          className={`${FIELD} w-40`}
          placeholder="Name"
        />
        <button type="button" className={BUTTON} aria-label={`Remove ${label}`} onClick={onRemove}>
          ×
        </button>
      </div>
      {problem && (
        <p role="alert" className="text-xs text-red-600 dark:text-red-400">
          {problem}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-3 text-xs">
        <select
          aria-label={`${label} action`}
          value={action.kind}
          onChange={(e) => {
            const kind = ACTIONS.find((a) => a.kind === e.target.value)?.kind;
            if (kind) onChange({ ...rule, action: actionOf(kind, action) });
          }}
          className="rounded border border-current/20 bg-transparent px-1 py-0.5"
        >
          {ACTIONS.map((a) => (
            <option key={a.kind} value={a.kind}>
              {a.label}
            </option>
          ))}
        </select>
        {action.kind === "highlight" && (
          <>
            <ColourPicker
              label={`${label} text colour`}
              value={action.style.fg}
              onChange={(fg) => style({ fg })}
            >
              Text
            </ColourPicker>
            <ColourPicker
              label={`${label} background`}
              value={action.style.bg}
              onChange={(bg) => style({ bg })}
            >
              Background
            </ColourPicker>
            <label className="flex items-center gap-1">
              <input
                type="checkbox"
                checked={action.style.line === true}
                onChange={(e) => style({ line: e.target.checked })}
              />
              Whole line
            </label>
          </>
        )}
        {action.kind === "send" && (
          <>
            <input
              aria-label={`${label} text to send`}
              value={action.text}
              onChange={(e) => onChange({ ...rule, action: { ...action, text: e.target.value } })}
              className="min-w-0 flex-1 rounded border border-current/20 bg-transparent px-1 py-0.5 font-mono"
              placeholder="yes (never a password: keep those in the vault)"
              spellCheck={false}
            />
            <label className="flex items-center gap-1">
              <input
                type="checkbox"
                checked={action.enter}
                onChange={(e) =>
                  onChange({ ...rule, action: { ...action, enter: e.target.checked } })
                }
              />
              Then Enter
            </label>
          </>
        )}
        <label className="flex items-center gap-1">
          <input
            type="checkbox"
            checked={rule.enabled}
            onChange={(e) => onChange({ ...rule, enabled: e.target.checked })}
          />
          On
        </label>
      </div>
    </li>
  );
}

/** A palette colour, an exact one, or none. */
function ColourPicker({
  label,
  value,
  onChange,
  children,
}: {
  label: string;
  value: string | null | undefined;
  onChange: (value: string | null) => void;
  children: string;
}) {
  const custom = typeof value === "string" && value.startsWith("#");
  return (
    <span className="flex items-center gap-1">
      {children}
      <select
        aria-label={label}
        value={custom ? "custom" : (value ?? "")}
        onChange={(e) => {
          const chosen = e.target.value;
          onChange(chosen === "" ? null : chosen === "custom" ? "#ff8800" : chosen);
        }}
        className="rounded border border-current/20 bg-transparent px-1 py-0.5"
      >
        <option value="">None</option>
        {PALETTE_NAMES.map((name, index) => (
          <option key={name} value={String(index)}>
            {name}
          </option>
        ))}
        <option value="custom">Exact…</option>
      </select>
      {custom && (
        <input
          type="color"
          aria-label={`${label}, exact`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="h-5 w-8"
        />
      )}
    </span>
  );
}
