// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Shares the main thread between terminals that are parsing output.
 *
 * Each xterm.js terminal parses its queue for up to 12 ms before it yields.
 * With several panes flooding at once those runs go back to back and leave
 * no time to paint or to handle a key: three panes running `yes` dropped 41%
 * of frames, and a keystroke took up to 91 ms to echo. The scheduler gives
 * one lane at a time a turn of about `turnMs`, in round-robin order.
 *
 * A turn keeps two `slice`-byte chunks with xterm.js and sends the next from
 * each chunk's callback. xterm.js runs that callback inside its own parse
 * loop and keeps parsing what is queued, so chunks within a turn cost no
 * timer hop; the second chunk keeps that true even if xterm.js only looked
 * at its queue between tasks. The turn ends once `turnMs` has passed since
 * its first chunk came back, which bounds it by time whatever the output is:
 * lines of `yes` or screen fills parse far slower per byte than long lines of
 * text. The clock starts at that first callback, not at the grant, so a
 * paint that runs before xterm.js gets to the turn does not use it up. A
 * turn overshoots by the chunks in flight, at most two.
 *
 * A turn never waits on xterm.js for good. If no chunk finishes for
 * `stallMs`, the turn ends anyway, and the lane gets no new turn until its
 * last chunk is back. xterm.js 6 does not recover from a throw inside its
 * parser: that terminal stops parsing, and without this rule every pane would
 * stop with it. A lane that is only slow, as in a hidden window whose timers
 * the browser throttles, loses nothing but the turn boundary.
 *
 * A small write to a lane with nothing queued skips the line. That is the
 * echo of a keystroke in a quiet pane, and it must not wait behind a flood.
 * While the page is hidden every write to such a lane skips the line: there
 * is no frame to protect, and the browser throttles timers so hard that
 * turns would slow every pane down. Order within a lane holds either way:
 * xterm.js parses its own writes in order, and a lane writes directly only
 * when it has nothing queued.
 *
 * Turns pass on through a posted task instead of from inside the xterm.js
 * callback, so a long chain of turns never nests timers deep enough for the
 * browser to clamp them to 4 ms.
 *
 * `push` is O(n / slice) for n bytes, as views over the same buffer. Each
 * chunk then costs O(1) amortized to send and to account for, and granting
 * or releasing a turn is O(1). The
 * scheduler holds O(lanes) entries, and a lane holds what the backend sent
 * and xterm.js has not parsed, which flow control caps at the high watermark.
 */
export class OutputScheduler {
  readonly slice: number;
  readonly direct: number;
  readonly turnMs: number;
  readonly stallMs: number;
  readonly now: () => number;
  readonly hidden: () => boolean;
  /** Runs `task` after `ms`; returns a function that cancels it. */
  readonly setTimer: (task: () => void, ms: number) => () => void;
  readonly #post: (task: () => void) => void;
  /** Lanes waiting for a turn; a `Set` iterates in insertion order. */
  readonly #waiting = new Set<Lane>();
  #active: Lane | undefined;
  #scheduled = false;

  constructor(options: {
    slice: number;
    direct: number;
    turnMs: number;
    stallMs: number;
    post?: (task: () => void) => void;
    now?: () => number;
    hidden?: () => boolean;
    setTimer?: (task: () => void, ms: number) => () => void;
  }) {
    const { slice, direct, turnMs, stallMs } = options;
    for (const [name, value] of Object.entries({ slice, direct })) {
      if (!Number.isInteger(value) || value < 1) {
        throw new RangeError(`${name} must be a positive integer, got ${value}`);
      }
    }
    for (const [name, value] of Object.entries({ turnMs, stallMs })) {
      if (!(value > 0)) {
        throw new RangeError(`${name} must be positive, got ${value}`);
      }
    }
    this.slice = slice;
    this.direct = direct;
    this.turnMs = turnMs;
    this.stallMs = stallMs;
    this.now = options.now ?? (() => performance.now());
    this.hidden =
      options.hidden ??
      (() => typeof document !== "undefined" && document.visibilityState === "hidden");
    this.setTimer =
      options.setTimer ??
      ((task, ms) => {
        const timer = setTimeout(task, ms);
        return () => clearTimeout(timer);
      });
    this.#post = options.post ?? taskPoster();
  }

  /**
   * Opens a lane. `write` hands one chunk to the terminal and calls `parsed`
   * once the terminal has parsed it; `onParsed` then receives its length.
   */
  lane(
    write: (chunk: Uint8Array, parsed: () => void) => void,
    onParsed: (bytes: number) => void,
  ): Lane {
    return new Lane(this, write, onParsed);
  }

  /** Whether `lane` holds no turn and waits for none. */
  isIdle(lane: Lane): boolean {
    return this.#active !== lane && !this.#waiting.has(lane);
  }

  /** Queues a lane that has chunks to send and none in flight. */
  enqueue(lane: Lane): void {
    if (this.#active === lane) return;
    this.#waiting.add(lane);
    this.#schedule();
  }

  /** Ends `lane`'s turn, or takes it out of the line. */
  release(lane: Lane): void {
    this.#waiting.delete(lane);
    if (this.#active !== lane) return;
    this.#active = undefined;
    if (lane.ready) this.#waiting.add(lane);
    this.#schedule();
  }

  #schedule(): void {
    if (this.#scheduled || this.#active || this.#waiting.size === 0) return;
    this.#scheduled = true;
    this.#post(() => {
      this.#scheduled = false;
      this.#grant();
    });
  }

  #grant(): void {
    if (this.#active) return;
    const next = this.#waiting.values().next();
    if (next.done) return;
    const lane = next.value;
    this.#waiting.delete(lane);
    this.#active = lane;
    lane.run();
  }
}

/** Chunks a turn keeps with xterm.js at once. */
const IN_FLIGHT = 2;
/**
 * How long the app's bytes wait for the session's output to leave a
 * sequence. Past that they go anyway, and the rest of the sequence, if it
 * ever comes, prints as text: better than a Clear that never happens.
 */
const HOLD_MS = 100;

const BEL = 0x07;
const CAN = 0x18;
const SUB = 0x1a;
const ESC = 0x1b;

/**
 * Where xterm.js's parser stands, folded to what an erase written there
 * would do: nothing in `ground`, and anywhere else it cuts short a sequence
 * the session has not finished, whose rest then prints as text. `string` is
 * a DCS, SOS, PM or APC string, which only ESC, CAN or SUB ends.
 */
type Parser = "ground" | "escape" | "escapeIntermediate" | "csi" | "osc" | "string";

/** The state after `byte` in `state`, for a byte other than ESC, CAN and SUB. */
function step(state: Parser, byte: number): Parser {
  switch (state) {
    case "escape":
      if (byte === 0x5b) return "csi";
      if (byte === 0x5d) return "osc";
      if (byte === 0x50 || byte === 0x58 || byte === 0x5e || byte === 0x5f) return "string";
      if (byte >= 0x20 && byte <= 0x2f) return "escapeIntermediate";
      // C0 controls run without leaving the sequence, and DEL is ignored.
      return byte < 0x20 || byte === 0x7f ? "escape" : "ground";
    case "escapeIntermediate":
      return byte < 0x30 || byte === 0x7f ? "escapeIntermediate" : "ground";
    case "csi":
      // A CSI ends at its final byte. A non-ASCII byte ends some CSI states
      // and not others; taking it as no end holds an erase longer, never
      // inside a sequence.
      return byte >= 0x40 && byte <= 0x7e ? "ground" : "csi";
    default:
      return state;
  }
}

/** Continuation bytes the last UTF-8 character lacks after `bytes`, given `missing` before. */
function utf8Missing(missing: number, bytes: Uint8Array): number {
  for (let back = 1; back <= Math.min(4, bytes.length); back += 1) {
    const byte = bytes[bytes.length - back]!;
    if ((byte & 0xc0) === 0x80) continue;
    const length = byte >= 0xf0 ? 4 : byte >= 0xe0 ? 3 : byte >= 0xc0 ? 2 : 1;
    return Math.max(0, length - back);
  }
  // Continuation bytes only.
  return Math.max(0, missing - bytes.length);
}

/**
 * Follows the session's bytes far enough to tell whether they end inside an
 * escape sequence, a control string or a UTF-8 character, across any number
 * of pushes. It mirrors the state table xterm.js 6 parses with
 * (`EscapeSequenceParser.ts`), folded to the states of `Parser`, for the
 * 7-bit forms only: a sequence opened by a C1 control goes unseen.
 *
 * ESC, CAN and SUB lead to one state from any other, so a push is stepped
 * through only after the last of them, and only until the parser is back in
 * the ground state or inside a string, which the bytes left cannot end.
 * O(bytes) per push, in native searches; the steps cover one sequence.
 */
export class SequenceTracker {
  #parser: Parser = "ground";
  #utf8Missing = 0;

  /** Whether the bytes so far end inside a sequence, a string or a UTF-8 character. */
  get inside(): boolean {
    return this.#parser !== "ground" || this.#utf8Missing > 0;
  }

  /** Follows `bytes`, which come right after the bytes followed so far. */
  feed(bytes: Uint8Array): void {
    this.#utf8Missing = utf8Missing(this.#utf8Missing, bytes);
    const esc = bytes.lastIndexOf(ESC);
    const cancel = Math.max(bytes.lastIndexOf(CAN), bytes.lastIndexOf(SUB));
    let state = this.#parser;
    let start = 0;
    if (esc > cancel) {
      state = "escape";
      start = esc + 1;
    } else if (cancel !== -1) {
      state = "ground";
      start = cancel + 1;
    }
    for (let at = start; at < bytes.length; at += 1) {
      if (state === "ground" || state === "string") break;
      if (state === "osc") {
        if (bytes.indexOf(BEL, at) !== -1) state = "ground";
        break;
      }
      state = step(state, bytes[at]!);
    }
    this.#parser = state;
  }
}

/** One terminal's queue of output on its way to xterm.js. */
export class Lane {
  readonly #scheduler: OutputScheduler;
  readonly #write: (chunk: Uint8Array, parsed: () => void) => void;
  readonly #onParsed: (bytes: number) => void;
  /** Chunks of at most `slice` bytes, oldest first, from `#head` on. */
  #chunks: Uint8Array[] = [];
  /** The queued chunks the app wrote itself, which are parsed but not counted. */
  readonly #local = new WeakSet<Uint8Array>();
  /** Where the bytes written so far, the session's and the app's, leave the parser. */
  readonly #sequence = new SequenceTracker();
  /** The app's bytes waiting for a push that ends outside a sequence. */
  #held: Uint8Array[] = [];
  /** Cancels the timer that writes `#held` anyway after HOLD_MS. */
  #cancelHold: (() => void) | undefined;
  #head = 0;
  /** Chunks handed to xterm.js in a turn and not parsed yet, at most IN_FLIGHT. */
  #inFlight = 0;
  /**
   * The open turn: when its first chunk came back (undefined until then) and
   * when a chunk last finished or the turn was granted.
   */
  #turn: { started: number | undefined; progress: number; cancelWatchdog: () => void } | undefined;
  #closed = false;

  constructor(
    scheduler: OutputScheduler,
    write: (chunk: Uint8Array, parsed: () => void) => void,
    onParsed: (bytes: number) => void,
  ) {
    this.#scheduler = scheduler;
    this.#write = write;
    this.#onParsed = onParsed;
  }

  get hasQueued(): boolean {
    return this.#head < this.#chunks.length;
  }

  /** Whether the lane can use a turn: chunks to send and none in flight. */
  get ready(): boolean {
    return !this.#closed && this.hasQueued && this.#inFlight === 0;
  }

  push(data: Uint8Array): void {
    if (this.#closed || data.byteLength === 0) return;
    if (
      !this.hasQueued &&
      this.#scheduler.isIdle(this) &&
      (data.byteLength <= this.#scheduler.direct || this.#scheduler.hidden())
    ) {
      this.#write(data, () => {
        if (!this.#closed) this.#onParsed(data.byteLength);
      });
    } else {
      const { slice } = this.#scheduler;
      for (let start = 0; start < data.byteLength; start += slice) {
        this.#chunks.push(data.subarray(start, start + slice));
      }
      if (this.ready) this.#scheduler.enqueue(this);
    }
    this.#sequence.feed(data);
    if (this.#held.length > 0 && !this.#sequence.inside) this.#releaseHeld();
  }

  /**
   * Writes `data`, which the app made, after everything pushed so far: at
   * the end of a whole push, never between the chunks a push was cut into,
   * which can fall inside an escape sequence. A push that itself ended
   * inside an escape sequence, a control string such as an OSC title, or a
   * UTF-8 character is finished by a later one, so `data` waits for the
   * first push that ends outside all of them, or HOLD_MS if none comes. Its
   * bytes reach no `onParsed`, because the session did not send them.
   */
  pushLocal(data: Uint8Array): void {
    if (this.#closed || data.byteLength === 0) return;
    if (this.#held.length > 0 || this.#sequence.inside) {
      this.#held.push(data);
      this.#cancelHold ??= this.#scheduler.setTimer(() => this.#releaseHeld(), HOLD_MS);
      return;
    }
    this.#placeLocal(data);
  }

  #releaseHeld(): void {
    this.#cancelHold?.();
    this.#cancelHold = undefined;
    for (const local of this.#held.splice(0)) this.#placeLocal(local);
  }

  #placeLocal(data: Uint8Array): void {
    // Bytes placed after HOLD_MS end the sequence they cut into, as they do in
    // xterm.js, so the next erase does not wait for that sequence too.
    this.#sequence.feed(data);
    if (!this.hasQueued && this.#scheduler.isIdle(this)) {
      this.#write(data, () => {});
      return;
    }
    this.#local.add(data);
    this.#chunks.push(data);
    if (this.ready) this.#scheduler.enqueue(this);
  }

  /** Starts a turn; called by the scheduler. */
  run(): void {
    if (!this.ready) {
      this.#scheduler.release(this);
      return;
    }
    const { now, setTimer, stallMs } = this.#scheduler;
    this.#turn = {
      started: undefined,
      progress: now(),
      cancelWatchdog: setTimer(() => this.#checkStall(), stallMs),
    };
    while (this.#turn && this.hasQueued && this.#inFlight < IN_FLIGHT) this.#sendNext();
  }

  /** Drops what is queued and gives up any turn. Callbacks after this are ignored. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#chunks = [];
    this.#head = 0;
    this.#held = [];
    this.#cancelHold?.();
    this.#cancelHold = undefined;
    this.#endTurn();
    this.#scheduler.release(this);
  }

  #sendNext(): void {
    const chunk = this.#chunks[this.#head]!;
    this.#head += 1;
    this.#compact();
    this.#inFlight += 1;
    const counted = this.#local.has(chunk) ? 0 : chunk.byteLength;
    try {
      this.#write(chunk, () => this.#parsed(counted));
    } catch (error) {
      // xterm.js throws only past its 50 MB discard limit, which flow
      // control keeps out of reach. The pane cannot go on either way.
      console.error("terminal refused output; closing its output lane", error);
      this.#inFlight -= 1;
      this.close();
    }
  }

  #parsed(bytes: number): void {
    if (this.#closed) return;
    this.#inFlight -= 1;
    try {
      if (bytes > 0) this.#onParsed(bytes);
    } finally {
      this.#continue();
    }
  }

  /** After a chunk is back: send another, end the turn, or queue for the next. */
  #continue(): void {
    const turn = this.#turn;
    if (!turn) {
      // The chunk outlived its turn, which ended on time or on a stall.
      if (this.ready) this.#scheduler.enqueue(this);
      return;
    }
    const at = this.#scheduler.now();
    turn.progress = at;
    turn.started ??= at;
    if (at - turn.started < this.#scheduler.turnMs) {
      if (this.hasQueued) {
        this.#sendNext();
        return;
      }
      if (this.#inFlight > 0) return;
    }
    this.#endTurn();
    this.#scheduler.release(this);
  }

  #checkStall(): void {
    const turn = this.#turn;
    if (!turn) return;
    const { now, setTimer, stallMs } = this.#scheduler;
    const quiet = now() - turn.progress;
    if (quiet < stallMs) {
      turn.cancelWatchdog = setTimer(() => this.#checkStall(), stallMs - quiet);
      return;
    }
    this.#turn = undefined;
    this.#scheduler.release(this);
  }

  #endTurn(): void {
    if (!this.#turn) return;
    this.#turn.cancelWatchdog();
    this.#turn = undefined;
  }

  /** Drops consumed slots once they are half the array: amortized O(1) per chunk. */
  #compact(): void {
    if (this.#head === this.#chunks.length) {
      this.#chunks = [];
      this.#head = 0;
    } else if (this.#head * 2 >= this.#chunks.length) {
      this.#chunks = this.#chunks.slice(this.#head);
      this.#head = 0;
    }
  }
}

/**
 * Returns a function that runs each task in a new macrotask, without the
 * nested-timer clamp of setTimeout. One channel serves every task.
 */
function taskPoster(): (task: () => void) => void {
  let channel: MessageChannel | undefined;
  const tasks: Array<() => void> = [];
  return (task) => {
    if (!channel) {
      channel = new MessageChannel();
      channel.port1.addEventListener("message", () => tasks.shift()?.());
      channel.port1.start();
    }
    tasks.push(task);
    channel.port2.postMessage(undefined);
  };
}

/**
 * Sizes for every terminal in the window. A 6 ms turn leaves most of a
 * 16.7 ms frame to paint and to handle input. 1 KiB chunks bound how far a
 * turn overshoots: on the 3-core macOS runner a 4 KiB chunk of wrapped text
 * took long enough that two in flight stretched turns past a frame, and 1 KiB
 * cost about 3% of throughput on Linux. No legitimate chunk takes 250 ms to
 * parse, and a stall that is only a slow lane costs it no more than a turn
 * boundary.
 */
export const outputScheduler = new OutputScheduler({
  slice: 1024,
  direct: 4 * 1024,
  turnMs: 6,
  stallMs: 250,
});
