// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The player's engine, free of the DOM and the worker plumbing so tests run
 * it directly: index a recording, build seek checkpoints, answer seeks with
 * a snapshot to show, hand out events to play, and search the text.
 *
 * Checkpoints are taken every {@link CHECKPOINT_PARSE_MS} of parse time (at
 * most every {@link CHECKPOINT_MAX_BYTES}), between escape sequences: the
 * cost of a seek is bounded by parse time, which a burst of empty lines
 * makes far longer per byte than plain text. A seek restores the nearest
 * checkpoint before the target into a scratch terminal, replays to the
 * target, and answers with that terminal's snapshot.
 */

import { SerializeAddon } from "@xterm/addon-serialize";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/headless";

import { hardenParser } from "@/features/terminal/repeat-guard.ts";

import { Cast, OUTPUT, RESIZE, parseSize } from "./cast.ts";
import { CHECKPOINT_SCROLLBACK, SNAPSHOT_FORMAT, atRest, snapshot } from "./snapshot.ts";

export const CHECKPOINT_PARSE_MS = 20;
export const CHECKPOINT_MAX_BYTES = 1024 * 1024;
/** Output between checkpoints, at least, as a multiple of the last one's size. */
export const CHECKPOINT_WEIGHT = 4;
/** Output per playback request at most, in characters. */
export const EVENTS_MAX_CHARS = 1024 * 1024;
/** Below this much output, a seek replays from the start: still fast. */
export const SMALL_CAST_BYTES = 3 * 1024 * 1024;
/** Characters of text shown around a search match. */
const CONTEXT = 40;
/** Output written per call while replaying. */
const BATCH = 16 * 1024;

export interface Checkpoint {
  /** The first event not in the snapshot. */
  readonly event: number;
  readonly cols: number;
  readonly rows: number;
  readonly data: string;
}

export interface Frame {
  /** The first event not shown. */
  readonly event: number;
  readonly cols: number;
  readonly rows: number;
  readonly snapshot: string;
}

export interface PlayEvent {
  readonly time: number;
  readonly kind: number;
  readonly data: string;
}

export interface Hit {
  readonly event: number;
  readonly time: number;
  readonly preview: string;
}

/** One terminal, of a size, as the live one was configured. */
interface Scratch {
  readonly term: Terminal;
  readonly serializer: SerializeAddon;
}

function scratch(cols: number, rows: number): Scratch {
  const term = new Terminal({
    cols,
    rows,
    scrollback: CHECKPOINT_SCROLLBACK,
    allowProposedApi: true,
  });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  // The recording holds what the host sent, unfiltered: the live pane's
  // guards apply here too.
  hardenParser(term);
  const serializer = new SerializeAddon();
  term.loadAddon(serializer);
  return { term, serializer };
}

/**
 * Writes `data` and returns once it is parsed. The synchronous write the
 * headless core has is used when present: it is about twice as fast.
 */
function writeNow(term: Terminal, data: string): Promise<void> {
  const core: unknown = Reflect.get(term, "_core");
  const sync: unknown =
    typeof core === "object" && core !== null ? Reflect.get(core, "writeSync") : undefined;
  if (typeof sync === "function") {
    Reflect.apply(sync, core, [data]);
    return Promise.resolve();
  }
  return new Promise((resolve) => term.write(data, resolve));
}

/** Keeps one checkpoint list in its saved text form. */
export function encodeCheckpoints(list: readonly Checkpoint[]): string {
  return JSON.stringify({ format: SNAPSHOT_FORMAT, checkpoints: list });
}

/** Checkpoints saved earlier, or `undefined` when they are not this format's. */
export function decodeCheckpoints(text: string, events: number): Checkpoint[] | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (typeof parsed !== "object" || parsed === null) return undefined;
  if (Reflect.get(parsed, "format") !== SNAPSHOT_FORMAT) return undefined;
  const list: unknown = Reflect.get(parsed, "checkpoints");
  if (!Array.isArray(list)) return undefined;
  const out: Checkpoint[] = [];
  let last = -1;
  for (const item of list) {
    if (typeof item !== "object" || item === null) return undefined;
    const event: unknown = Reflect.get(item, "event");
    const cols: unknown = Reflect.get(item, "cols");
    const rows: unknown = Reflect.get(item, "rows");
    const data: unknown = Reflect.get(item, "data");
    // Sizes as a terminal takes them: a checkpoint saying a billion
    // columns is not one this player made.
    if (
      typeof event !== "number" ||
      typeof cols !== "number" ||
      typeof rows !== "number" ||
      typeof data !== "string" ||
      !Number.isInteger(event) ||
      !Number.isInteger(cols) ||
      !Number.isInteger(rows) ||
      cols < 1 ||
      cols > 1000 ||
      rows < 1 ||
      rows > 500 ||
      event <= last ||
      event > events
    ) {
      return undefined;
    }
    last = event;
    out.push({ event, cols, rows, data });
  }
  return out;
}

// Matching controls is the point of these.
// oxlint-disable-next-line no-control-regex
const OSC = /\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/gu;
// oxlint-disable-next-line no-control-regex
const CSI = /\u001b\[[0-?]*[ -/]*[@-~]/gu;
// oxlint-disable-next-line no-control-regex
const ESC = /\u001b[@-Z\\-_]/gu;
// oxlint-disable-next-line no-control-regex
const CONTROLS = /[\u0000-\u0008\u000b-\u001f\u007f]/gu;

/** Strips escape sequences and control characters, for searching. */
function plain(text: string): string {
  return text.replace(OSC, "").replace(CSI, "").replace(ESC, "").replace(CONTROLS, "");
}

export class PlayerCore {
  readonly cast: Cast;
  readonly #decoder = new TextDecoder();
  #checkpoints: Checkpoint[] = [];
  /** Checkpoints cover events before this; the build goes on from here. */
  #built = 0;
  #building: Promise<void> | undefined;
  /** The scratch terminal for seeks, and the event it has reached. */
  #seek: { scratch: Scratch; event: number } | undefined;
  #stopped = false;

  readonly #spacing: { parseMs: number; maxBytes: number; weight: number };

  /**
   * `options` tunes the checkpoint spacing and the size below which no
   * checkpoints are built; tests make them small.
   */
  constructor(
    bytes: Uint8Array,
    saved?: string,
    options: { small?: number; parseMs?: number; maxBytes?: number; weight?: number } = {},
  ) {
    const small = options.small ?? SMALL_CAST_BYTES;
    this.#spacing = {
      parseMs: options.parseMs ?? CHECKPOINT_PARSE_MS,
      maxBytes: options.maxBytes ?? CHECKPOINT_MAX_BYTES,
      weight: options.weight ?? CHECKPOINT_WEIGHT,
    };
    this.cast = Cast.parse(bytes);
    const outputBytes = this.cast.count === 0 ? 0 : (this.cast.before[this.cast.count - 1] ?? 0);
    const decoded = saved === undefined ? undefined : decodeCheckpoints(saved, this.cast.count);
    if (decoded) {
      this.#checkpoints = decoded;
      this.#built = this.cast.count;
    } else if (outputBytes < small) {
      // Small: a seek replays from the start, under 150 ms.
      this.#built = this.cast.count;
    }
  }

  get checkpoints(): readonly Checkpoint[] {
    return this.#checkpoints;
  }

  /** How far the checkpoints reach, as a share of the events, 0 to 1. */
  get progress(): number {
    return this.cast.count === 0 ? 1 : this.#built / this.cast.count;
  }

  /** Whether the checkpoints came from this run, and so may be saved. */
  get builtHere(): boolean {
    return this.#builtHere;
  }

  #builtHere = false;

  stop(): void {
    this.#stopped = true;
  }

  /**
   * Builds checkpoints over the whole recording, reporting progress, and
   * yielding between slices so seeks are answered meanwhile.
   */
  build(onProgress: (share: number) => void): Promise<void> {
    this.#building ??= this.#build(onProgress);
    return this.#building;
  }

  async #build(onProgress: (share: number) => void): Promise<void> {
    if (this.#built >= this.cast.count) {
      onProgress(1);
      return;
    }
    this.#builtHere = true;
    const { width, height } = this.cast.header;
    const work = scratch(width, height);
    let cols = width;
    let rows = height;
    let sinceParse = 0;
    let sinceBytes = 0;
    // Each checkpoint waits for its predecessor's size, times the weight,
    // in output: checkpoints weigh at most about a quarter of the cast.
    let lastSize = 0;
    let reported = performance.now();
    let slice = performance.now();
    for (let i = 0; i < this.cast.count; i += 1) {
      if (this.#stopped) return;
      const began = performance.now();
      const kind = this.cast.kinds[i];
      const data = this.cast.data(i, this.#decoder);
      if (kind === RESIZE) {
        const size = parseSize(data);
        if (size) {
          work.term.resize(size.cols, size.rows);
          cols = size.cols;
          rows = size.rows;
        }
      } else if (kind === OUTPUT) {
        for (let at = 0; at < data.length; at += BATCH) {
          // oxlint-disable-next-line no-await-in-loop -- parsed in order
          await writeNow(work.term, data.slice(at, at + BATCH));
        }
        sinceBytes += data.length;
      }
      sinceParse += performance.now() - began;
      if (
        (sinceParse >= this.#spacing.parseMs || sinceBytes >= this.#spacing.maxBytes) &&
        sinceBytes >= lastSize * this.#spacing.weight &&
        atRest(work.term)
      ) {
        const snap = snapshot(work.term, work.serializer);
        if (snap !== undefined) {
          this.#checkpoints.push({ event: i + 1, cols, rows, data: snap });
          lastSize = snap.length;
          sinceParse = 0;
          sinceBytes = 0;
          // A seek to the first checkpoint, early: the first seek runs the
          // restore and snapshot code cold, and a user's should not.
          if (this.#checkpoints.length === 1) {
            // oxlint-disable-next-line no-await-in-loop -- once
            await this.seek(i + 1);
          }
        }
      }
      this.#built = i + 1;
      const now = performance.now();
      if (now - reported > 200) {
        reported = now;
        onProgress(this.progress);
      }
      // Let seeks in between, every few milliseconds.
      if (now - slice > 8) {
        // oxlint-disable-next-line no-await-in-loop -- yields to waiting work
        await new Promise((resolve) => setTimeout(resolve, 0));
        slice = performance.now();
      }
    }
    work.term.dispose();
    this.#built = this.cast.count;
    onProgress(1);
  }

  /** The last checkpoint at or before event `target`. O(log c). */
  #checkpointBefore(target: number): Checkpoint | undefined {
    let low = 0;
    let high = this.#checkpoints.length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if ((this.#checkpoints[mid]?.event ?? 0) <= target) low = mid + 1;
      else high = mid;
    }
    return low === 0 ? undefined : this.#checkpoints[low - 1];
  }

  /**
   * What the screen is just before event `target`: restores the nearest
   * checkpoint, replays the rest, and snapshots. A target in the middle of
   * an escape sequence moves on to the end of it.
   */
  async seek(target: number): Promise<Frame> {
    const goal = Math.max(0, Math.min(this.cast.count, target));
    const from = this.#checkpointBefore(Math.min(goal, this.#built));
    const reused = this.#seek;
    // Going on from where the last seek left off beats restoring when it is
    // closer than the checkpoint.
    let state: { scratch: Scratch; event: number };
    if (reused && reused.event <= goal && reused.event >= (from?.event ?? 0)) {
      state = reused;
    } else {
      reused?.scratch.term.dispose();
      const { width, height } = this.cast.header;
      const fresh = scratch(from?.cols ?? width, from?.rows ?? height);
      if (from) await writeNow(fresh.term, from.data);
      state = { scratch: fresh, event: from?.event ?? 0 };
    }
    this.#seek = state;
    let at = state.event;
    const term = state.scratch.term;
    while (at < this.cast.count && (at < goal || !atRest(term))) {
      const kind = this.cast.kinds[at];
      const data = this.cast.data(at, this.#decoder);
      if (kind === RESIZE) {
        const size = parseSize(data);
        if (size) term.resize(size.cols, size.rows);
      } else if (kind === OUTPUT) {
        for (let i = 0; i < data.length; i += BATCH) {
          // oxlint-disable-next-line no-await-in-loop -- parsed in order
          await writeNow(term, data.slice(i, i + BATCH));
        }
      }
      at += 1;
    }
    state.event = at;
    // A recording that ends in the middle of a sequence (cut off by a stop
    // or a crash) still shows its screen; the cut sequence is dropped.
    const snap =
      snapshot(term, state.scratch.serializer) ??
      snapshot(term, state.scratch.serializer, CHECKPOINT_SCROLLBACK, true) ??
      "";
    return { event: at, cols: term.cols, rows: term.rows, snapshot: snap };
  }

  /** Events from `from`, up to `seconds` of recording after `fromTime`. */
  events(from: number, until: number): PlayEvent[] {
    const out: PlayEvent[] = [];
    let chars = 0;
    for (let i = Math.max(0, from); i < this.cast.count && chars < EVENTS_MAX_CHARS; i += 1) {
      const time = this.cast.times[i] ?? 0;
      if (time > until) break;
      const data = this.cast.data(i, this.#decoder);
      chars += data.length;
      out.push({ time, kind: this.cast.kinds[i] ?? OUTPUT, data });
    }
    return out;
  }

  /**
   * Where `query` shows in the output, case aside, first at most `limit`.
   * A match across two events counts, in the later one. O(n).
   */
  async search(
    query: string,
    limit = 500,
    stale: () => boolean = () => false,
  ): Promise<Hit[] | undefined> {
    const needle = query.toLowerCase();
    if (needle.length === 0) return [];
    const hits: Hit[] = [];
    // The text just before, for matches across events and for context.
    let recent = "";
    let slice = performance.now();
    for (let i = 0; i < this.cast.count && hits.length < limit; i += 1) {
      // A slice at a time: seeks go in between, and a newer search ends this.
      if (performance.now() - slice > 8) {
        // oxlint-disable-next-line no-await-in-loop -- yields to waiting work
        await new Promise((resolve) => setTimeout(resolve, 0));
        if (stale()) return undefined;
        slice = performance.now();
      }
      if (this.cast.kinds[i] !== OUTPUT) continue;
      const text = plain(this.cast.data(i, this.#decoder));
      const joined = recent + text;
      // Counted where it ends, so a match is found once.
      const from = Math.max(0, recent.length - needle.length + 1);
      const found = joined.toLowerCase().indexOf(needle, from);
      if (found >= 0) {
        const start = Math.max(0, found - CONTEXT);
        hits.push({
          event: i,
          time: this.cast.times[i] ?? 0,
          preview: joined
            .slice(start, found + needle.length + CONTEXT)
            .replace(/\s+/gu, " ")
            .trim(),
        });
      }
      recent = joined.slice(-Math.max(CONTEXT, needle.length - 1));
    }
    return hits;
  }
}
