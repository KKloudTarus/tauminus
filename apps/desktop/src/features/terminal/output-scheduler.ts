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

/** One terminal's queue of output on its way to xterm.js. */
export class Lane {
  readonly #scheduler: OutputScheduler;
  readonly #write: (chunk: Uint8Array, parsed: () => void) => void;
  readonly #onParsed: (bytes: number) => void;
  /** Chunks of at most `slice` bytes, oldest first, from `#head` on. */
  #chunks: Uint8Array[] = [];
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
      return;
    }
    const { slice } = this.#scheduler;
    for (let start = 0; start < data.byteLength; start += slice) {
      this.#chunks.push(data.subarray(start, start + slice));
    }
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
    this.#endTurn();
    this.#scheduler.release(this);
  }

  #sendNext(): void {
    const chunk = this.#chunks[this.#head]!;
    this.#head += 1;
    this.#compact();
    this.#inFlight += 1;
    try {
      this.#write(chunk, () => this.#parsed(chunk.byteLength));
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
      this.#onParsed(bytes);
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
 * 16.7 ms frame to paint and to handle input, and 4 KiB chunks bound how far
 * a turn overshoots. No legitimate 4 KiB chunk takes 250 ms to parse, and a
 * stall that is only a slow lane costs it no more than a turn boundary.
 */
export const outputScheduler = new OutputScheduler({
  slice: 4 * 1024,
  direct: 4 * 1024,
  turnMs: 6,
  stallMs: 250,
});
