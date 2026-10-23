// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * An asciicast v2 recording, indexed for playback without a JavaScript
 * object per event: an hour of heavy output is two million events. The
 * file stays one byte array; the index keeps each event's time, kind, and
 * where its line is, in typed arrays. An event's text is decoded only when
 * it is played. O(n) to index, O(log n) to find the event at a time.
 */

export interface CastHeader {
  readonly width: number;
  readonly height: number;
  /** Seconds since the Unix epoch, when recording began. */
  readonly timestamp: number;
  readonly title: string;
}

export const OUTPUT = 0;
export const RESIZE = 1;

export class Cast {
  readonly header: CastHeader;
  /** Events, in order. */
  readonly count: number;
  /** Seconds since the start, per event. */
  readonly times: Float64Array;
  readonly kinds: Uint8Array;
  /** Where each event's line starts and ends in `bytes`. */
  readonly starts: Uint32Array;
  readonly ends: Uint32Array;
  /** Output bytes before each event, as the cast holds them; for checkpoints. */
  readonly before: Float64Array;
  readonly bytes: Uint8Array;

  private constructor(
    header: CastHeader,
    count: number,
    arrays: {
      times: Float64Array;
      kinds: Uint8Array;
      starts: Uint32Array;
      ends: Uint32Array;
      before: Float64Array;
    },
    bytes: Uint8Array,
  ) {
    this.header = header;
    this.count = count;
    this.times = arrays.times;
    this.kinds = arrays.kinds;
    this.starts = arrays.starts;
    this.ends = arrays.ends;
    this.before = arrays.before;
    this.bytes = bytes;
  }

  /** Seconds from the start to the last event. */
  get duration(): number {
    return this.count === 0 ? 0 : (this.times[this.count - 1] ?? 0);
  }

  /** Reads `bytes`. Lines that are not events are skipped. */
  static parse(bytes: Uint8Array): Cast {
    const newline = 0x0a;
    const decoder = new TextDecoder();
    let at = bytes.indexOf(newline);
    if (at < 0) at = bytes.length;
    const header = parseHeader(decoder.decode(bytes.subarray(0, at)));
    // Lines are at least `[0, "o", ""]` long: room for that many events.
    let capacity = 1024;
    let times = new Float64Array(capacity);
    let kinds = new Uint8Array(capacity);
    let starts = new Uint32Array(capacity);
    let ends = new Uint32Array(capacity);
    let before = new Float64Array(capacity);
    let count = 0;
    let output = 0;
    let line = at + 1;
    while (line < bytes.length) {
      let end = bytes.indexOf(newline, line);
      if (end < 0) end = bytes.length;
      const event = scanEvent(bytes, line, end);
      if (event) {
        if (count === capacity) {
          capacity *= 2;
          times = growFloats(times, capacity);
          kinds = growBytes(kinds, capacity);
          starts = growOffsets(starts, capacity);
          ends = growOffsets(ends, capacity);
          before = growFloats(before, capacity);
        }
        times[count] = event.time;
        kinds[count] = event.kind;
        starts[count] = line;
        ends[count] = end;
        before[count] = output;
        if (event.kind === OUTPUT) output += end - line;
        count += 1;
      }
      line = end + 1;
    }
    return new Cast(
      header,
      count,
      {
        times: times.subarray(0, count),
        kinds: kinds.subarray(0, count),
        starts: starts.subarray(0, count),
        ends: ends.subarray(0, count),
        before: before.subarray(0, count),
      },
      bytes,
    );
  }

  /** The first event after `seconds`: events before it have happened. O(log n). */
  indexAt(seconds: number): number {
    let low = 0;
    let high = this.count;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if ((this.times[mid] ?? 0) <= seconds) low = mid + 1;
      else high = mid;
    }
    return low;
  }

  /** Event `index`'s data: output text, or `COLSxROWS`. */
  data(index: number, decoder: TextDecoder): string {
    const line = decoder.decode(
      this.bytes.subarray(this.starts[index] ?? 0, this.ends[index] ?? 0),
    );
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      // A last line torn by a crash: nothing of it plays.
      return "";
    }
    return Array.isArray(parsed) && typeof parsed[2] === "string" ? parsed[2] : "";
  }
}

function growFloats(array: Float64Array, size: number): Float64Array<ArrayBuffer> {
  const bigger = new Float64Array(size);
  bigger.set(array);
  return bigger;
}

function growBytes(array: Uint8Array, size: number): Uint8Array<ArrayBuffer> {
  const bigger = new Uint8Array(size);
  bigger.set(array);
  return bigger;
}

function growOffsets(array: Uint32Array, size: number): Uint32Array<ArrayBuffer> {
  const bigger = new Uint32Array(size);
  bigger.set(array);
  return bigger;
}

function parseHeader(line: string): CastHeader {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    throw new Error("not an asciicast recording");
  }
  if (typeof parsed !== "object" || parsed === null) throw new Error("not an asciicast recording");
  const header: object = parsed;
  const field = (name: string): unknown => Reflect.get(header, name);
  const version = field("version");
  const width = field("width");
  const height = field("height");
  if (version !== 2 || typeof width !== "number" || typeof height !== "number") {
    throw new Error("only asciicast v2 recordings play here");
  }
  const timestamp = field("timestamp");
  const title = field("title");
  return {
    width: clampSize(width, 1, 1000),
    height: clampSize(height, 1, 500),
    timestamp: typeof timestamp === "number" ? timestamp : 0,
    title: typeof title === "string" ? title : "",
  };
}

function clampSize(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, Math.floor(value)));
}

/**
 * Reads `[seconds, "o"` or `[seconds, "r"` at the start of a line, the way
 * the recorder writes it, without decoding the rest. `undefined` for any
 * other line.
 */
function scanEvent(
  bytes: Uint8Array,
  start: number,
  end: number,
): { time: number; kind: number } | undefined {
  if (bytes[start] !== 0x5b) return undefined;
  let at = start + 1;
  let number = "";
  while (at < end) {
    const b = bytes[at] ?? 0;
    if (
      (b >= 0x30 && b <= 0x39) ||
      b === 0x2e ||
      b === 0x65 ||
      b === 0x45 ||
      b === 0x2d ||
      b === 0x2b
    ) {
      number += String.fromCharCode(b);
      at += 1;
    } else break;
  }
  const time = Number(number);
  if (number === "" || !Number.isFinite(time) || time < 0) return undefined;
  while (at < end && (bytes[at] === 0x2c || bytes[at] === 0x20)) at += 1;
  if (bytes[at] !== 0x22 || bytes[at + 2] !== 0x22) return undefined;
  const kind = bytes[at + 1];
  if (kind === 0x6f) return { time, kind: OUTPUT };
  if (kind === 0x72) return { time, kind: RESIZE };
  return undefined;
}

/** `COLSxROWS` as the recorder writes a resize, within the sizes a terminal takes. */
export function parseSize(data: string): { cols: number; rows: number } | undefined {
  const match = /^(\d{1,4})x(\d{1,4})$/u.exec(data);
  if (!match) return undefined;
  const cols = Number(match[1]);
  const rows = Number(match[2]);
  if (cols < 1 || rows < 1 || cols > 1000 || rows > 500) return undefined;
  return { cols, rows };
}
