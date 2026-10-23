// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { memo, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { BookmarkSpec, RecordingView, RecordingsClient } from "@/ipc/index.ts";

import type { Hit } from "./player-core.ts";
import { PlayerController } from "./player-controller.ts";
import type { PlayerEngine } from "./engine.ts";

const FIELD =
  "rounded border border-current/20 bg-transparent px-2 py-1.5 outline-none focus:border-current/50";
const BUTTON = "rounded px-2 py-1 hover:bg-current/10 disabled:opacity-40";
const SPEEDS = [0.5, 1, 2, 4, 8, 16] as const;

/** `m:ss`, or `h:mm:ss`. */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${rest}` : `${m}:${rest}`;
}

interface PlayerProps {
  recording: RecordingView;
  client: RecordingsClient;
  /** Opens the vault, then tries again. */
  onUnlock: (retry: () => void) => void;
  onBack: () => void;
  /** For tests: a worker stand-in. */
  engine?: () => PlayerEngine;
}

/** Plays a recording, with seek, speed, search, and bookmarks. */
export function Player({ recording, client, onUnlock, onBack, engine }: PlayerProps) {
  // Ends itself once no view watches it.
  const [controller] = useState(() => new PlayerController(client, recording, engine));
  const state = useSyncExternalStore(controller.subscribe, controller.getState);

  const { status } = state;
  if (status.kind === "loading") {
    return <p className="p-4 opacity-70">Opening the recording…</p>;
  }
  if (status.kind === "locked") {
    return (
      <div className="p-4">
        <p className="mb-2">Recordings are sealed to the vault: unlock it to play one.</p>
        <button
          type="button"
          className={BUTTON}
          onClick={() => onUnlock(() => void controller.load())}
        >
          Unlock the vault
        </button>
      </div>
    );
  }
  if (status.kind === "failed") {
    return (
      <p role="alert" className="p-4 text-red-600 dark:text-red-400">
        Could not play the recording: {status.text}
      </p>
    );
  }
  const { duration } = status;
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2" data-testid="player">
      <div className="flex items-center gap-2">
        <button type="button" className={BUTTON} onClick={onBack}>
          ← Recordings
        </button>
        <span className="min-w-0 flex-1 truncate font-medium">{recording.title}</span>
        {state.built < 1 && (
          <span className="text-xs opacity-60" data-testid="index-progress">
            Preparing seek {Math.round(state.built * 100)}%
          </span>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-auto rounded bg-black p-1">
        <div
          ref={(host) => {
            controller.attach(host ?? undefined);
            return () => controller.attach(undefined);
          }}
          data-testid="player-screen"
        />
      </div>
      <div className="flex items-center gap-2">
        <button
          type="button"
          className={BUTTON}
          aria-label={state.playing ? "Pause" : "Play"}
          onClick={() => (state.playing ? controller.pause() : controller.play())}
        >
          {state.playing ? "❚❚" : "▶"}
        </button>
        <span className="w-28 text-xs tabular-nums" data-testid="player-time">
          {clock(state.time)} / {clock(duration)}
        </span>
        <input
          type="range"
          aria-label="Position"
          min={0}
          max={Math.max(duration, 0.001)}
          step="any"
          value={Math.min(state.time, duration)}
          onChange={(e) => void controller.seek(Number(e.target.value))}
          className="flex-1"
          data-last-seek-ms={state.lastSeekMs}
          data-seeks={state.seeks}
        />
        <select
          aria-label="Speed"
          value={state.speed}
          onChange={(e) => controller.setSpeed(Number(e.target.value))}
          className={FIELD}
        >
          {SPEEDS.map((s) => (
            <option key={s} value={s}>
              {s}×
            </option>
          ))}
        </select>
        <label className="flex items-center gap-1 text-xs">
          <input
            type="checkbox"
            checked={state.skipIdle}
            onChange={(e) => controller.setSkipIdle(e.target.checked)}
          />
          Skip idle
        </label>
      </div>
      <div className="grid max-h-56 min-h-0 grid-cols-2 gap-3">
        <Search controller={controller} />
        <Bookmarks recording={recording} client={client} controller={controller} />
      </div>
    </div>
  );
}

/** Re-rendered by its own input only, not by the clock. */
const Search = memo(function Search({ controller }: { controller: PlayerController }) {
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<{ query: string; hits: Hit[] } | undefined>(undefined);
  const hits = query.trim() !== "" && found?.query === query ? found.hits : undefined;
  useEffect(() => {
    if (query.trim() === "") return undefined;
    let current = true;
    const timer = setTimeout(() => {
      controller
        .search(query)
        .then((result) => {
          if (current) setFound({ query, hits: result });
        })
        .catch(() => {});
    }, 150);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [controller, query]);
  return (
    <div className="flex min-h-0 flex-col gap-1">
      <input
        type="search"
        aria-label="Search the recording"
        placeholder="Search the output"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        spellCheck={false}
        className={FIELD}
      />
      <ul className="min-h-0 flex-1 overflow-auto text-xs" aria-label="Search results">
        {hits?.length === 0 && <li className="opacity-60">Not found</li>}
        {hits?.map((hit) => (
          <li key={hit.event}>
            <button
              type="button"
              className="w-full truncate rounded px-1 text-left hover:bg-current/10"
              onClick={() => void controller.seek(hit.time)}
            >
              <span className="tabular-nums opacity-60">{clock(hit.time)}</span> {hit.preview}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
});

/**
 * The recording's bookmarks. Each save replaces the whole list, so none is
 * made from a list that did not load, and saves go one after another, each
 * from the list the one before it saved.
 */
const Bookmarks = memo(function Bookmarks({
  recording,
  client,
  controller,
}: {
  recording: RecordingView;
  client: RecordingsClient;
  controller: PlayerController;
}) {
  const [marks, setMarks] = useState<BookmarkSpec[] | undefined>(undefined);
  const [label, setLabel] = useState("");
  const [comment, setComment] = useState("");
  const [error, setError] = useState<string | undefined>(undefined);
  const commentId = useId();
  const saving = useRef<Promise<void>>(Promise.resolve());
  const latest = useRef<BookmarkSpec[]>([]);
  useEffect(() => {
    let current = true;
    client
      .bookmarks(recording.id)
      .then((list) => {
        if (!current) return;
        latest.current = list;
        setMarks(list);
      })
      .catch((failure: unknown) => {
        if (current) setError(`Could not read the bookmarks: ${messageOf(failure)}`);
      });
    return () => {
      current = false;
    };
  }, [client, recording.id]);
  const save = (change: (list: BookmarkSpec[]) => BookmarkSpec[]) => {
    saving.current = saving.current.then(async () => {
      const next = change(latest.current).toSorted((a, b) => (a.atMs ?? 0) - (b.atMs ?? 0));
      try {
        await client.saveBookmarks(recording.id, next);
        latest.current = next;
        setMarks(next);
        setError(undefined);
      } catch (failure) {
        setError(messageOf(failure));
      }
    });
  };
  return (
    <div className="flex min-h-0 flex-col gap-1">
      <form
        aria-label="New bookmark"
        className="flex gap-1"
        onSubmit={(e) => {
          e.preventDefault();
          if (label.trim() === "" || !marks) return;
          const mark = { atMs: Math.round(controller.time * 1000), label: label.trim(), comment };
          save((list) => [...list, mark]);
          setLabel("");
          setComment("");
        }}
      >
        <input
          aria-label="Bookmark"
          placeholder="Bookmark this moment"
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          className={`${FIELD} min-w-0 flex-1`}
        />
        <button type="submit" className={BUTTON} disabled={label.trim() === "" || !marks}>
          Add
        </button>
      </form>
      <textarea
        id={commentId}
        aria-label="Comment"
        placeholder="Comment (optional)"
        value={comment}
        onChange={(e) => setComment(e.target.value)}
        rows={1}
        className={FIELD}
      />
      {error && (
        <p role="alert" className="text-xs text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
      <ul className="min-h-0 flex-1 overflow-auto text-xs" aria-label="Bookmarks">
        {marks?.map((mark) => (
          <li key={`${mark.atMs}-${mark.label}-${mark.comment}`} className="flex items-start gap-1">
            <button
              type="button"
              className="min-w-0 flex-1 rounded px-1 text-left hover:bg-current/10"
              onClick={() => void controller.seek((mark.atMs ?? 0) / 1000)}
            >
              <span className="tabular-nums opacity-60">{clock((mark.atMs ?? 0) / 1000)}</span>{" "}
              <span className="font-medium">{mark.label}</span>
              {mark.comment && <span className="block opacity-70">{mark.comment}</span>}
            </button>
            <button
              type="button"
              className={BUTTON}
              aria-label={`Remove bookmark ${mark.label}`}
              onClick={() =>
                save((list) =>
                  list.filter(
                    (m) =>
                      !(
                        m.atMs === mark.atMs &&
                        m.label === mark.label &&
                        m.comment === mark.comment
                      ),
                  ),
                )
              }
            >
              ×
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
});
