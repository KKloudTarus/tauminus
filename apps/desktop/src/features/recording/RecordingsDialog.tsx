// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useEffect, useId, useRef, useState } from "react";

import { messageOf } from "@/features/hosts/hosts-store.ts";
import type { RecordingView, RecordingsClient } from "@/ipc/index.ts";

import type { PlayerEngine } from "./engine.ts";
import { Player, clock } from "./Player.tsx";

const BUTTON = "btn btn-ghost btn-sm";

/** `1.2 MB`, from bytes. */
export function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

interface RecordingsDialogProps {
  client: RecordingsClient;
  onClose: () => void;
  onUnlock: (retry: () => void) => void;
  /** For tests. */
  engine?: () => PlayerEngine;
}

type List =
  | { kind: "loading" }
  | { kind: "ready"; recordings: RecordingView[] }
  | { kind: "failed"; text: string };

/** The recordings kept here, and the player. A modal dialog. */
export function RecordingsDialog({ client, onClose, onUnlock, engine }: RecordingsDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [list, setList] = useState<List>({ kind: "loading" });
  const [playing, setPlaying] = useState<RecordingView | undefined>(undefined);
  const [confirming, setConfirming] = useState<string | undefined>(undefined);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
  }, []);

  const reload = useCallback(() => {
    client
      .list()
      .then((recordings) => setList({ kind: "ready", recordings }))
      .catch((failure: unknown) => setList({ kind: "failed", text: messageOf(failure) }));
  }, [client]);
  useEffect(() => {
    reload();
  }, [reload]);

  // The player and a long list want the height; a message alone does not.
  const roomy = playing !== undefined || (list.kind === "ready" && list.recordings.length > 0);
  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className={`panel modal flex flex-col p-4 sm:mt-[4dvh] sm:max-h-[92dvh] ${
        roomy ? "h-[min(860px,92dvh)] w-[min(1100px,96vw)]" : "w-[min(560px,96vw)]"
      }`}
    >
      <h2 id={titleId} className="mb-3 font-semibold">
        Recordings
      </h2>
      {playing ? (
        <Player
          recording={playing}
          client={client}
          onUnlock={onUnlock}
          onBack={() => {
            setPlaying(undefined);
            reload();
          }}
          {...(engine ? { engine } : {})}
        />
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          {list.kind === "loading" && <p className="text-muted">Loading…</p>}
          {list.kind === "failed" && (
            <p role="alert" className="text-error">
              Could not list recordings: {list.text}
            </p>
          )}
          {list.kind === "ready" && list.recordings.length === 0 && (
            <p className="text-muted">
              No recordings yet. Start one on a pane from the command palette: “Record this pane”.
            </p>
          )}
          {list.kind === "ready" && list.recordings.length > 0 && (
            <ul aria-label="Recordings" className="divide-y divide-line">
              {list.recordings.map((recording) => (
                <li key={recording.id} className="flex items-center gap-3 py-2">
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium">{recording.title}</div>
                    <div className="text-xs text-faint">
                      {new Date(recording.startedMs ?? 0).toLocaleString()} ·{" "}
                      {clock((recording.durationMs ?? 0) / 1000)} · {size(recording.bytes ?? 0)}
                      {!recording.complete && " · still recording, or cut short"}
                      {recording.truncated && " · stopped early"}
                    </div>
                  </div>
                  <button
                    type="button"
                    className={BUTTON}
                    aria-label={`Play ${recording.title}`}
                    onClick={() => setPlaying(recording)}
                  >
                    Play
                  </button>
                  {confirming === recording.id ? (
                    <button
                      type="button"
                      className={`${BUTTON} text-danger`}
                      onClick={() => {
                        client
                          .remove(recording.id)
                          .then(reload)
                          .catch((failure: unknown) =>
                            setList({ kind: "failed", text: messageOf(failure) }),
                          );
                        setConfirming(undefined);
                      }}
                    >
                      Delete for good
                    </button>
                  ) : (
                    <button
                      type="button"
                      className={BUTTON}
                      aria-label={`Delete ${recording.title}`}
                      onClick={() => setConfirming(recording.id)}
                    >
                      Delete
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      <div className="mt-3 flex justify-end">
        <button type="button" className={BUTTON} onClick={() => dialogRef.current?.close()}>
          Close
        </button>
      </div>
    </dialog>
  );
}
