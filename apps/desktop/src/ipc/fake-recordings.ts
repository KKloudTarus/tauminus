// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Recordings in memory for the e2e build: what a fake session prints while
// recorded becomes an asciicast, as the backend would write it. A recording
// is "sealed" to the fake vault: playing back needs it open.

import type { ActiveRecording, BookmarkSpec, GridSize, RecordingView } from "./bindings.ts";
import { fakeOutputTaps } from "./fake.ts";
import type { RecordingsClient } from "./recordings.ts";
import { SessionCommandError } from "./sessions.ts";

interface Kept {
  view: RecordingView;
  lines: string[];
  started: number;
  checkpoints: string | null;
  bookmarks: BookmarkSpec[];
}

const encoder = new TextEncoder();

/**
 * The fake, `seed`, which keeps a recording made elsewhere, and `cut`,
 * which stops every recording as the backend does when the disk fails.
 */
export type FakeRecordings = RecordingsClient & {
  seed(title: string, cast: string): string;
  cut(): void;
};

/** `locked` says whether the fake vault is locked now. */
export function fakeRecordings(locked: () => boolean): FakeRecordings {
  const kept = new Map<string, Kept>();
  const active = new Map<number, string>();
  let next = 1;
  const needsOpen = () => {
    if (locked()) {
      throw new SessionCommandError({ code: "vault_locked", message: "the vault is locked" });
    }
  };
  const get = (id: string) => {
    const found = kept.get(id);
    if (!found) throw new SessionCommandError({ code: "not_found", message: `no recording ${id}` });
    return found;
  };
  const stop = (session: number): RecordingView | null => {
    const id = active.get(session);
    if (id === undefined) return null;
    active.delete(session);
    fakeOutputTaps.delete(session);
    const recording = get(id);
    recording.view = {
      ...recording.view,
      complete: true,
      durationMs: Date.now() - recording.started,
    };
    return recording.view;
  };
  return {
    cut() {
      for (const session of active.keys()) stop(session);
    },
    seed(title, cast) {
      const id = String(next).padStart(32, "0");
      next += 1;
      const lines = cast.trimEnd().split("\n");
      const last: unknown = JSON.parse(lines.at(-1) ?? "[0]");
      const seconds = Array.isArray(last) && typeof last[0] === "number" ? last[0] : 0;
      kept.set(id, {
        view: {
          id,
          title,
          host: null,
          startedMs: Date.now(),
          durationMs: seconds * 1000,
          bytes: cast.length,
          truncated: false,
          complete: true,
        },
        lines,
        started: Date.now(),
        checkpoints: null,
        bookmarks: [],
      });
      return id;
    },
    async start(
      session: number,
      size: GridSize,
      title: string,
      host: string | null,
      initial: string | null,
    ) {
      if (active.has(session)) {
        throw new SessionCommandError({
          code: "invalid_argument",
          message: "this pane is already recording",
        });
      }
      const id = String(next).padStart(32, "0");
      next += 1;
      const started = Date.now();
      const lines = [
        JSON.stringify({
          version: 2,
          width: size.cols,
          height: size.rows,
          timestamp: Math.floor(started / 1000),
          title,
        }),
      ];
      if (initial) lines.push(JSON.stringify([0, "o", initial]));
      const view: RecordingView = {
        id,
        title,
        host,
        startedMs: started,
        durationMs: 0,
        bytes: 0,
        truncated: false,
        complete: false,
      };
      const recording: Kept = { view, lines, started, checkpoints: null, bookmarks: [] };
      kept.set(id, recording);
      active.set(session, id);
      fakeOutputTaps.set(session, (text) => {
        const at = (Date.now() - started) / 1000;
        lines.push(JSON.stringify([Number(at.toFixed(6)), "o", text]));
        recording.view = {
          ...recording.view,
          bytes: (recording.view.bytes ?? 0) + encoder.encode(text).length,
        };
      });
      return view;
    },
    stop: async (session) => stop(session),
    active: async (): Promise<ActiveRecording[]> =>
      [...active].map(([session, id]) => ({ session, recording: get(id).view })),
    list: async () =>
      [...kept.values()]
        .map((k) => k.view)
        .toSorted((a, b) => (b.startedMs ?? 0) - (a.startedMs ?? 0)),
    async read(id, onChunk) {
      needsOpen();
      const recording = get(id);
      onChunk(encoder.encode(`${recording.lines.join("\n")}\n`));
      return recording.view.complete;
    },
    async checkpoints(id) {
      needsOpen();
      return get(id).checkpoints;
    },
    async saveCheckpoints(id, checkpoints) {
      get(id).checkpoints = checkpoints;
    },
    async bookmarks(id) {
      needsOpen();
      return get(id).bookmarks;
    },
    async saveBookmarks(id, bookmarks) {
      get(id).bookmarks = bookmarks;
    },
    async remove(id) {
      if ([...active.values()].includes(id)) {
        throw new SessionCommandError({
          code: "invalid_argument",
          message: "this pane is already recording",
        });
      }
      get(id);
      kept.delete(id);
    },
  };
}

/**
 * A recording of an hour, made up: `bytes` of colourful output spread over
 * 3600 s, for the seek test.
 */
export function fakeHourLongCast(bytes: number): string {
  const lines = [
    JSON.stringify({
      version: 2,
      width: 120,
      height: 36,
      timestamp: 1_700_000_000,
      title: "an hour",
    }),
  ];
  let made = 0;
  let n = 0;
  const events = Math.max(1, Math.floor(bytes / 400));
  while (made < bytes) {
    const time = (n / events) * 3600;
    const text =
      n % 50 === 0
        ? `\u001b[?1049h\u001b[2J\u001b[1;20r\u001b(0lqqqqk\u001b(B top ${n}\u001b[r\u001b[?1049l`
        : `\u001b[1;3${n % 8}m${n}\u001b[0m ${"journal line: service ok ".repeat(12)}\r\n`;
    lines.push(JSON.stringify([Number(time.toFixed(6)), "o", text]));
    made += text.length;
    n += 1;
  }
  return `${lines.join("\n")}\n`;
}
