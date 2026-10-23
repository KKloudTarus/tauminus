// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import {
  type ActiveRecording,
  type BookmarkSpec,
  type GridSize,
  type RecordingView,
  commands,
} from "./bindings.ts";
import { SessionCommandError } from "./sessions.ts";

export type { ActiveRecording, BookmarkSpec, RecordingView };

/**
 * Session recordings. Recording works with the vault locked once its key
 * exists; playing back, bookmarks, and checkpoints need it open
 * (`vault_locked`).
 */
export interface RecordingsClient {
  /** `initial` is the screen now, serialized, so playback starts from it. */
  start(
    session: number,
    size: GridSize,
    title: string,
    host: string | null,
    initial: string | null,
  ): Promise<RecordingView>;
  stop(session: number): Promise<RecordingView | null>;
  active(): Promise<ActiveRecording[]>;
  list(): Promise<RecordingView[]>;
  /** The asciicast text, piece by piece; `true` when it ended cleanly. */
  read(id: string, onChunk: (bytes: Uint8Array) => void): Promise<boolean>;
  /** The checkpoints the player saved last, or `null`. */
  checkpoints(id: string): Promise<string | null>;
  saveCheckpoints(id: string, checkpoints: string): Promise<void>;
  bookmarks(id: string): Promise<BookmarkSpec[]>;
  saveBookmarks(id: string, bookmarks: BookmarkSpec[]): Promise<void>;
  remove(id: string): Promise<void>;
}

type Result<T> = Promise<
  | { status: "ok"; data: T }
  | { status: "error"; error: { code: SessionCommandError["code"]; message: string } }
>;

async function unwrap<T>(result: Result<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") throw new SessionCommandError(outcome.error);
  return outcome.data;
}

const decoder = new TextDecoder();

export const tauriRecordings: RecordingsClient = {
  start: (session, size, title, host, initial) =>
    unwrap(commands.recordingStart(session, size, title, host, initial)),
  stop: async (session) => commands.recordingStop(session),
  active: async () => commands.recordingsActive(),
  list: () => unwrap(commands.recordingsList()),
  read: async (id, onChunk) => {
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buffer) => onChunk(new Uint8Array(buffer));
    const done = await unwrap(commands.recordingRead(id, channel));
    return done.complete;
  },
  checkpoints: async (id) => {
    let text: string | null = null;
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buffer) => {
      text = decoder.decode(buffer);
    };
    const sent = await unwrap(commands.recordingCheckpoints(id, channel));
    return sent ? text : null;
  },
  saveCheckpoints: async (id, checkpoints) => {
    await unwrap(commands.recordingSaveCheckpoints(id, checkpoints));
  },
  bookmarks: (id) => unwrap(commands.recordingBookmarks(id)),
  saveBookmarks: async (id, bookmarks) => {
    await unwrap(commands.recordingSaveBookmarks(id, bookmarks));
  },
  remove: async (id) => {
    await unwrap(commands.recordingDelete(id));
  },
};
