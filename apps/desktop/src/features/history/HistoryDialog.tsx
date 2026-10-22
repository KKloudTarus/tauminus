// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type ReactNode, useEffect, useId, useMemo, useRef, useState } from "react";

import { messageOf } from "@/features/hosts/hosts-store.ts";
import {
  type ExitFilterSpec,
  type HistoryClient,
  type HistoryHitSpec,
  type HostView,
  SessionCommandError,
} from "@/ipc/index.ts";

const FIELD =
  "rounded border border-current/20 bg-transparent px-2 py-1.5 outline-none focus:border-current/50";

/** Results asked for per search; the backend allows up to 500. */
export const PAGE = 50;
/** Typing pauses this long before a search runs. */
export const DEBOUNCE_MS = 150;

const HOUR = 60 * 60 * 1000;
const SINCE: readonly { label: string; ms: number | null }[] = [
  { label: "Any time", ms: null },
  { label: "Last 24 hours", ms: 24 * HOUR },
  { label: "Last 7 days", ms: 7 * 24 * HOUR },
  { label: "Last 30 days", ms: 30 * 24 * HOUR },
];

/** `""` means local and ad hoc sessions; `null` all hosts. */
type HostFilter = string | null;

interface HistoryDialogProps {
  client: HistoryClient;
  hosts: readonly HostView[];
  onClose: () => void;
  /** Types the command into the focused terminal, without running it. */
  onInsert: (command: string) => void;
  onCopy: (text: string) => void;
  /** Opens the vault, then searches again. */
  onUnlock: (retry: () => void) => void;
  now?: () => number;
}

type Results =
  | { kind: "loading" }
  | { kind: "ready"; hits: HistoryHitSpec[] }
  | { kind: "locked" }
  | { kind: "failed"; text: string };

/** Searches the command history across hosts. A modal dialog. */
export function HistoryDialog({
  client,
  hosts,
  onClose,
  onInsert,
  onCopy,
  onUnlock,
  now = Date.now,
}: HistoryDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const titleId = useId();
  const [text, setText] = useState("");
  const [host, setHost] = useState<HostFilter>(null);
  const [since, setSince] = useState(0);
  const [exit, setExit] = useState<ExitFilterSpec>("any");
  const [results, setResults] = useState<Results>({ kind: "loading" });
  const [generation, setGeneration] = useState(0);
  // A search per pause in typing, not per keystroke.
  const [deferredText, setDeferredText] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setDeferredText(text), text === "" ? 0 : DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [text]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
    searchRef.current?.focus();
  }, []);

  // The search to run: changes with the filters, and with `generation`,
  // which a retry after an unlock or a clear bumps.
  const query = useMemo(() => {
    const window = SINCE[since]?.ms ?? null;
    return {
      text: deferredText,
      host,
      fromMs: window === null ? null : now() - window,
      toMs: null,
      exit,
      limit: PAGE,
      generation,
    };
  }, [deferredText, host, since, exit, generation, now]);

  useEffect(() => {
    // Only the newest search's answer is shown.
    let current = true;
    const { generation: _again, ...search } = query;
    client
      .search(search)
      .then((hits) => {
        if (current) setResults({ kind: "ready", hits });
      })
      .catch((failure: unknown) => {
        if (!current) return;
        if (failure instanceof SessionCommandError && failure.code === "vault_locked") {
          setResults({ kind: "locked" });
        } else {
          setResults({ kind: "failed", text: messageOf(failure) });
        }
      });
    return () => {
      current = false;
    };
  }, [client, query]);

  const retry = () => setGeneration((g) => g + 1);

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className="top-[6%] m-0 mx-auto flex h-[min(720px,88vh)] w-[min(820px,94vw)] flex-col rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl backdrop:bg-black/30"
    >
      <h2 id={titleId} className="mb-3 font-semibold">
        Command history
      </h2>
      <div className="mb-3 flex flex-wrap gap-2">
        <input
          ref={searchRef}
          type="search"
          aria-label="Search commands and output"
          placeholder="Search commands and output"
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
          className={`${FIELD} min-w-48 flex-1`}
        />
        <select
          aria-label="Host"
          value={host ?? "\u0000all"}
          onChange={(e) => setHost(e.target.value === "\u0000all" ? null : e.target.value)}
          className={FIELD}
        >
          <option value={"\u0000all"}>All hosts</option>
          <option value="">Local and ad hoc</option>
          {hosts.map((h) => (
            <option key={h.id} value={h.id}>
              {h.label}
            </option>
          ))}
        </select>
        <select
          aria-label="Time"
          value={since}
          onChange={(e) => setSince(Number(e.target.value))}
          className={FIELD}
        >
          {SINCE.map((s, n) => (
            <option key={s.label} value={n}>
              {s.label}
            </option>
          ))}
        </select>
        <select
          aria-label="Exit status"
          value={exit}
          onChange={(e) => {
            const chosen = (["any", "success", "failure"] as const).find(
              (v) => v === e.target.value,
            );
            if (chosen) setExit(chosen);
          }}
          className={FIELD}
        >
          <option value="any">Any exit</option>
          <option value="success">Succeeded</option>
          <option value="failure">Failed</option>
        </select>
      </div>
      <div className="min-h-32 flex-1 overflow-auto">
        <Body
          results={results}
          client={client}
          now={now}
          onInsert={(command) => {
            onInsert(command);
            dialogRef.current?.close();
          }}
          onCopy={onCopy}
          onUnlock={() => onUnlock(retry)}
        />
      </div>
      <div className="mt-3 flex items-center gap-2">
        <ClearButton client={client} host={host} hosts={hosts} onCleared={retry} />
        <span className="flex-1" />
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

function Body({
  results,
  client,
  now,
  onInsert,
  onCopy,
  onUnlock,
}: {
  results: Results;
  client: HistoryClient;
  now: () => number;
  onInsert: (command: string) => void;
  onCopy: (text: string) => void;
  onUnlock: () => void;
}) {
  switch (results.kind) {
    case "loading":
      return <p className="opacity-70">Searching…</p>;
    case "locked":
      return (
        <div className="py-6 text-center">
          <p className="mb-3 opacity-80">
            The history is encrypted with the vault. Unlock it to search; commands are still
            recorded while it is locked.
          </p>
          <button
            type="button"
            onClick={onUnlock}
            className="rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90"
          >
            Unlock the vault
          </button>
        </div>
      );
    case "failed":
      return (
        <p role="alert" className="text-red-600 dark:text-red-400">
          {results.text}
        </p>
      );
    case "ready":
      if (results.hits.length === 0) {
        return <p className="opacity-70">No commands match.</p>;
      }
      return (
        <ul aria-label="Commands" className="divide-y divide-current/10">
          {results.hits.map((hit) => (
            <Hit
              key={hit.id}
              hit={hit}
              client={client}
              now={now}
              onInsert={onInsert}
              onCopy={onCopy}
            />
          ))}
        </ul>
      );
    default:
      return null;
  }
}

function Hit({
  hit,
  client,
  now,
  onInsert,
  onCopy,
}: {
  hit: HistoryHitSpec;
  client: HistoryClient;
  now: () => number;
  onInsert: (command: string) => void;
  onCopy: (text: string) => void;
}) {
  const [output, setOutput] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();
  return (
    <li className="py-2">
      <div className="flex items-start gap-2">
        <code className="min-w-0 flex-1 font-mono text-xs break-all whitespace-pre-wrap">
          {hit.command}
        </code>
        <ExitBadge exit={hit.exit} />
      </div>
      <div className="mt-1 flex flex-wrap items-center gap-x-3 text-xs opacity-70">
        <span>{hit.label}</span>
        <time dateTime={new Date(hit.at * 1000).toISOString()}>{ago(now() - hit.at * 1000)}</time>
      </div>
      {hit.snippet && (
        <p className="mt-1 font-mono text-xs break-all whitespace-pre-wrap opacity-80">
          <Marked text={hit.snippet} />
        </p>
      )}
      <div className="mt-1 flex gap-2 text-xs">
        <button
          type="button"
          onClick={() => onInsert(hit.command)}
          className="rounded border border-current/20 px-2 py-0.5 hover:bg-current/10"
        >
          Insert
        </button>
        <button
          type="button"
          onClick={() => onCopy(hit.command)}
          className="rounded border border-current/20 px-2 py-0.5 hover:bg-current/10"
        >
          Copy
        </button>
        <button
          type="button"
          aria-expanded={output !== undefined}
          onClick={() => {
            if (output !== undefined) {
              setOutput(undefined);
              return;
            }
            client
              .output(hit.id)
              .then((text) => setOutput(text ?? ""))
              .catch((failure: unknown) => setError(messageOf(failure)));
          }}
          className="rounded border border-current/20 px-2 py-0.5 hover:bg-current/10"
        >
          {output === undefined ? "Show output" : "Hide output"}
        </button>
      </div>
      {error && (
        <p role="alert" className="mt-1 text-xs text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
      {output !== undefined && (
        <pre className="mt-2 max-h-64 overflow-auto rounded bg-current/5 p-2 font-mono text-xs whitespace-pre-wrap">
          {output || "(no output)"}
        </pre>
      )}
    </li>
  );
}

function ExitBadge({ exit }: { exit: number | null }) {
  if (exit === null) return null;
  const ok = exit === 0;
  return (
    <span
      title={`exit ${exit}`}
      className={`shrink-0 rounded px-1.5 text-xs ${
        ok
          ? "bg-green-600/15 text-green-700 dark:text-green-400"
          : "bg-red-600/15 text-red-700 dark:text-red-400"
      }`}
    >
      {ok ? "ok" : `exit ${exit}`}
    </span>
  );
}

/** Text with the backend's match markers (U+0001 … U+0002) shown as marks. */
export function Marked({ text }: { text: string }) {
  const parts: ReactNode[] = [];
  let rest = text;
  let key = 0;
  while (rest) {
    const start = rest.indexOf("\u0001");
    if (start < 0) {
      parts.push(rest);
      break;
    }
    const end = rest.indexOf("\u0002", start);
    parts.push(rest.slice(0, start));
    const marked = end < 0 ? rest.slice(start + 1) : rest.slice(start + 1, end);
    parts.push(
      <mark key={key++} className="rounded bg-yellow-300/50 text-current">
        {marked}
      </mark>,
    );
    rest = end < 0 ? "" : rest.slice(end + 1);
  }
  return <>{parts}</>;
}

/** "3 min ago", "2 h ago", "5 d ago". */
export function ago(ms: number): string {
  const minutes = Math.max(0, Math.round(ms / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} d ago`;
}

function ClearButton({
  client,
  host,
  hosts,
  onCleared,
}: {
  client: HistoryClient;
  host: HostFilter;
  hosts: readonly HostView[];
  onCleared: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | undefined>();
  const target =
    host === null
      ? "all hosts"
      : host === ""
        ? "local and ad hoc sessions"
        : (hosts.find((h) => h.id === host)?.label ?? host);
  return (
    <>
      <button
        type="button"
        onClick={() => {
          if (!confirming) {
            setConfirming(true);
            return;
          }
          setConfirming(false);
          client
            .clear(host)
            .then(onCleared)
            .catch((failure: unknown) => setError(messageOf(failure)));
        }}
        onBlur={() => setConfirming(false)}
        className="rounded border border-red-500/50 px-3 py-1.5 text-red-600 hover:bg-red-500/10 dark:text-red-400"
      >
        {confirming ? `Forget the history of ${target}?` : "Forget history…"}
      </button>
      {error && (
        <span role="alert" className="text-xs text-red-600 dark:text-red-400">
          {error}
        </span>
      )}
    </>
  );
}
