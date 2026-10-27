// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { OutputScheduler } from "./output-scheduler.ts";

/** A scheduler whose posted tasks and timers run only when the test says so. */
function setup(sizes: { turnMs?: number; stallMs?: number } = {}) {
  const page = { hidden: false };
  const tasks: Array<() => void> = [];
  const timers = new Map<number, { at: number; task: () => void }>();
  let nextTimer = 0;
  const clock = { now: 0 };
  const scheduler = new OutputScheduler({
    slice: 4,
    direct: 2,
    turnMs: sizes.turnMs ?? 10,
    stallMs: sizes.stallMs ?? 100,
    post: (task) => tasks.push(task),
    now: () => clock.now,
    hidden: () => page.hidden,
    setTimer: (task, ms) => {
      nextTimer += 1;
      const id = nextTimer;
      timers.set(id, { at: clock.now + ms, task });
      return () => timers.delete(id);
    },
  });
  const runTasks = () => {
    while (tasks.length > 0) tasks.shift()!();
  };
  const runOne = () => tasks.shift()?.();
  /** Moves the clock on and fires the timers that come due. */
  const advance = (ms: number) => {
    clock.now += ms;
    for (const [id, timer] of timers) {
      if (timer.at <= clock.now) {
        timers.delete(id);
        timer.task();
      }
    }
  };
  return { scheduler, runTasks, runOne, clock, advance, timers, page };
}

/**
 * A terminal that parses one chunk, oldest first, each time the test calls
 * `parseOne`, taking `ms` on the clock, the way xterm.js calls back after
 * each chunk.
 */
function terminal(scheduler: OutputScheduler, name: string, log: string[], clock = { now: 0 }) {
  const unparsed: Array<() => void> = [];
  const parsedBytes: number[] = [];
  const lane = scheduler.lane(
    (chunk, parsed) => {
      log.push(`${name}:${new TextDecoder().decode(chunk)}`);
      unparsed.push(parsed);
    },
    (bytes) => parsedBytes.push(bytes),
  );
  const parseOne = (ms = 0) => {
    clock.now += ms;
    unparsed.shift()?.();
  };
  /** Parses until nothing is waiting, chunks sent meanwhile included. */
  const parseAll = (ms = 0) => {
    while (unparsed.length > 0) parseOne(ms);
  };
  return { lane, parseOne, parseAll, parsedBytes, unparsed };
}

const bytes = (text: string) => new TextEncoder().encode(text);

describe("OutputScheduler", () => {
  it("writes a small chunk to a quiet lane at once", () => {
    const { scheduler } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    a.lane.push(bytes("ok"));
    expect(log).toEqual(["a:ok"]);
    a.parseAll();
    expect(a.parsedBytes).toEqual([2]);
  });

  it("writes the app's own bytes after a whole push, and does not count them", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    a.lane.push(bytes("\x1b[1;3"));
    a.lane.push(bytes("4mok"));
    // Pushed while the first chunk is out: it waits for the second push.
    runTasks();
    a.lane.pushLocal(bytes("!"));
    a.parseAll();
    expect(log).toEqual(["a:\x1b[1;", "a:3", "a:4mok", "a:!"]);
    expect(a.parsedBytes).toEqual([4, 1, 4]);
  });

  it("writes the app's own bytes to a quiet lane at once, uncounted", () => {
    const { scheduler } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    a.lane.pushLocal(bytes("local bytes"));
    expect(log).toEqual(["a:local bytes"]);
    a.parseAll();
    expect(a.parsedBytes).toEqual([]);
  });

  it("keeps two chunks with the terminal and sends the next from each callback", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    a.lane.push(bytes("abcdefghijkl"));
    expect(log).toEqual([]);
    runTasks();
    expect(log).toEqual(["a:abcd", "a:efgh"]);
    a.parseOne();
    expect(log).toEqual(["a:abcd", "a:efgh", "a:ijkl"]);
    a.parseAll();
    expect(a.parsedBytes).toEqual([4, 4, 4]);
  });

  it("ends a turn by time and lets the next lane go", () => {
    const { scheduler, runTasks, clock } = setup({ turnMs: 10 });
    const log: string[] = [];
    const a = terminal(scheduler, "a", log, clock);
    const b = terminal(scheduler, "b", log, clock);
    a.lane.push(bytes("a".repeat(40)));
    b.lane.push(bytes("b".repeat(40)));
    runTasks();
    // Each chunk takes 4 ms. The clock starts when the first is back at
    // 4 ms; chunks come back at 8 and 12 ms inside the turn, and at 16 ms it
    // is over.
    for (let i = 0; i < 4; i += 1) a.parseOne(4);
    expect(log).toEqual(["a:aaaa", "a:aaaa", "a:aaaa", "a:aaaa", "a:aaaa"]);
    runTasks();
    expect(log.slice(5)).toEqual(["b:bbbb", "b:bbbb"]);
    // a's last chunk comes back during b's turn; a queues for the next one.
    a.parseOne(1);
    for (let i = 0; i < 4; i += 1) b.parseOne(4);
    runTasks();
    expect(log.slice(7)).toEqual(["b:bbbb", "b:bbbb", "b:bbbb", "a:aaaa", "a:aaaa"]);
  });

  it("starts the turn's clock when the first chunk is back, not at the grant", () => {
    const { scheduler, runTasks, clock } = setup({ turnMs: 10 });
    const log: string[] = [];
    const a = terminal(scheduler, "a", log, clock);
    a.lane.push(bytes("a".repeat(40)));
    runTasks();
    // A 30 ms paint runs before xterm.js gets to the turn.
    clock.now += 30;
    a.parseOne(1);
    a.parseOne(1);
    expect(log).toHaveLength(4);
  });

  it("gives a lane whose output parses slowly per byte about the same time, not the same bytes", () => {
    const { scheduler, runTasks, clock } = setup({ turnMs: 10 });
    const log: string[] = [];
    const slow = terminal(scheduler, "s", log, clock);
    const fast = terminal(scheduler, "f", log, clock);
    slow.lane.push(bytes("s".repeat(80)));
    fast.lane.push(bytes("f".repeat(80)));
    runTasks();
    // Slow chunks take 20 ms: the second is back past the turn.
    slow.parseOne(20);
    slow.parseOne(20);
    runTasks();
    // Fast chunks take 1 ms: ten more go out before the turn is over.
    for (let i = 0; i < 11; i += 1) fast.parseOne(1);
    expect(log.filter((l) => l.startsWith("s:"))).toHaveLength(3);
    expect(log.filter((l) => l.startsWith("f:"))).toHaveLength(12);
  });

  it("stops sending once the turn is over, even when the terminal parses inside write", () => {
    const { scheduler, runOne, clock } = setup({ turnMs: 10 });
    const log: string[] = [];
    const a = scheduler.lane(
      (chunk, done) => {
        log.push(`a:${chunk.byteLength}`);
        clock.now += 6;
        done();
      },
      () => {},
    );
    a.push(bytes("a".repeat(40)));
    runOne();
    // Back at 6 ms, where the clock starts, then 12 and 18: the third ends
    // the turn, and the next turn waits for its own task.
    expect(log).toEqual(["a:4", "a:4", "a:4"]);
    runOne();
    expect(log).toHaveLength(6);
  });

  it("holds the turn while the last chunk is still out", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    const b = terminal(scheduler, "b", log);
    a.lane.push(bytes("aaaaaaaa"));
    b.lane.push(bytes("bbbb"));
    runTasks();
    a.parseOne();
    runTasks();
    expect(log).toEqual(["a:aaaa", "a:aaaa"]);
    a.parseOne();
    runTasks();
    expect(log).toEqual(["a:aaaa", "a:aaaa", "b:bbbb"]);
  });

  it("lets a quiet lane's echo through while another lane holds the turn", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    const b = terminal(scheduler, "b", log);
    a.lane.push(bytes("aaaaaaaa"));
    runTasks();
    b.lane.push(bytes("x"));
    expect(log).toEqual(["a:aaaa", "a:aaaa", "b:x"]);
  });

  it("keeps a lane's order: a small write queues behind what is waiting", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    a.lane.push(bytes("aaaaaaaaaa"));
    a.lane.push(bytes("z"));
    runTasks();
    a.parseAll();
    expect(log).toEqual(["a:aaaa", "a:aaaa", "a:aa", "a:z"]);
  });

  it("writes straight through while the page is hidden", () => {
    const { scheduler, runTasks, page } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    const b = terminal(scheduler, "b", log);
    page.hidden = true;
    a.lane.push(bytes("aaaaaaaaaa"));
    b.lane.push(bytes("bbbbbbbbbb"));
    expect(log).toEqual(["a:aaaaaaaaaa", "b:bbbbbbbbbb"]);
    runTasks();
    expect(log).toHaveLength(2);
  });

  it("ends a stalled turn so other lanes keep going", () => {
    const { scheduler, runTasks, advance } = setup({ stallMs: 100 });
    const log: string[] = [];
    const stuck = terminal(scheduler, "s", log);
    const b = terminal(scheduler, "b", log);
    stuck.lane.push(bytes("s".repeat(12)));
    b.lane.push(bytes("bbbb"));
    runTasks();
    expect(log).toEqual(["s:ssss", "s:ssss"]);
    // The terminal never calls back, as xterm.js after a throw in its parser.
    advance(99);
    runTasks();
    expect(log).toHaveLength(2);
    advance(1);
    runTasks();
    expect(log).toEqual(["s:ssss", "s:ssss", "b:bbbb"]);
    b.parseAll();
    // The stuck lane gets no new turn while its chunks are still out.
    b.lane.push(bytes("bbbbbb"));
    runTasks();
    expect(log.slice(3)).toEqual(["b:bbbb", "b:bb"]);
  });

  it("gives a slow lane its turns back once its late chunks return", () => {
    const { scheduler, runTasks, advance } = setup({ stallMs: 100 });
    const log: string[] = [];
    const slow = terminal(scheduler, "s", log);
    slow.lane.push(bytes("s".repeat(12)));
    runTasks();
    advance(150);
    slow.parseOne();
    runTasks();
    expect(log).toHaveLength(2);
    slow.parseOne();
    runTasks();
    expect(log).toEqual(["s:ssss", "s:ssss", "s:ssss"]);
    expect(slow.parsedBytes).toEqual([4, 4]);
  });

  it("keeps a turn that is slow but moving", () => {
    const { scheduler, runTasks, advance, clock } = setup({ turnMs: 1000, stallMs: 100 });
    const log: string[] = [];
    const a = terminal(scheduler, "a", log, clock);
    const b = terminal(scheduler, "b", log, clock);
    a.lane.push(bytes("a".repeat(24)));
    b.lane.push(bytes("bbbb"));
    runTasks();
    for (let i = 0; i < 4; i += 1) {
      advance(80);
      a.parseOne();
    }
    runTasks();
    expect(log.filter((l) => l.startsWith("b:"))).toEqual([]);
  });

  it("ends the turn even when the ack callback throws", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const unparsed: Array<() => void> = [];
    const a = scheduler.lane(
      (chunk, parsed) => {
        log.push(`a:${chunk.byteLength}`);
        unparsed.push(parsed);
      },
      () => {
        throw new Error("ack failed");
      },
    );
    const b = terminal(scheduler, "b", log);
    a.push(bytes("aaaa"));
    b.lane.push(bytes("bbbb"));
    runTasks();
    expect(() => unparsed.shift()!()).toThrow("ack failed");
    runTasks();
    expect(log).toEqual(["a:4", "b:bbbb"]);
  });

  it("closes a lane whose terminal throws on write and keeps the others going", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const errors: unknown[] = [];
    const original = console.error;
    console.error = (...args: unknown[]) => errors.push(args);
    try {
      const broken = scheduler.lane(
        () => {
          throw new Error("discarded");
        },
        () => {},
      );
      const b = terminal(scheduler, "b", log);
      broken.push(bytes("x".repeat(12)));
      b.lane.push(bytes("bbbb"));
      runTasks();
      expect(log).toEqual(["b:bbbb"]);
      expect(errors).toHaveLength(1);
      expect(broken.ready).toBe(false);
    } finally {
      console.error = original;
    }
  });

  it("passes the turn on when the lane holding it closes", () => {
    const { scheduler, runTasks, timers } = setup();
    const log: string[] = [];
    const a = terminal(scheduler, "a", log);
    const b = terminal(scheduler, "b", log);
    a.lane.push(bytes("aaaaaaaaaaaa"));
    b.lane.push(bytes("bbbb"));
    runTasks();
    a.lane.close();
    runTasks();
    expect(log).toEqual(["a:aaaa", "a:aaaa", "b:bbbb"]);
    // A terminal disposed mid-turn may still call back; nothing counts it.
    a.parseAll();
    expect(a.parsedBytes).toEqual([]);
    a.lane.push(bytes("late"));
    expect(log).toHaveLength(3);
    b.parseAll();
    expect(timers.size).toBe(0);
  });

  it("copes with a terminal that parses during the write call", () => {
    const { scheduler, runTasks } = setup();
    const log: string[] = [];
    const parsed: number[] = [];
    const a = scheduler.lane(
      (chunk, done) => {
        log.push(`a:${chunk.byteLength}`);
        done();
      },
      (n) => parsed.push(n),
    );
    a.push(bytes("aaaaaaaaaa"));
    runTasks();
    expect(log).toEqual(["a:4", "a:4", "a:2"]);
    expect(parsed).toEqual([4, 4, 2]);
  });

  it("rejects sizes that cannot work", () => {
    const sizes = { slice: 4, direct: 2, turnMs: 1, stallMs: 10 };
    expect(() => new OutputScheduler({ ...sizes, slice: 0 })).toThrow(RangeError);
    expect(() => new OutputScheduler({ ...sizes, direct: 1.5 })).toThrow(RangeError);
    expect(() => new OutputScheduler({ ...sizes, turnMs: 0 })).toThrow(RangeError);
    expect(() => new OutputScheduler({ ...sizes, stallMs: Number.NaN })).toThrow(RangeError);
  });
});
