// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * One recording being played: loading it into the worker, the terminal it
 * shows in, seeking, and playing at a speed. Outside React, as the terminal
 * controller is; the view subscribes.
 */

import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/xterm";

import { messageOf } from "@/features/hosts/hosts-store.ts";
import { type RecordingView, type RecordingsClient, SessionCommandError } from "@/ipc/index.ts";

import { OUTPUT, RESIZE, parseSize } from "./cast.ts";
import { type Loaded, PlayerEngine, eventAt } from "./engine.ts";
import type { Hit, PlayEvent } from "./player-core.ts";
import { hardenParser } from "@/features/terminal/repeat-guard.ts";

import { CHECKPOINT_SCROLLBACK } from "./snapshot.ts";

/** Pauses longer than this play as this long when idle is skipped, in s. */
const IDLE_CAP = 1;
/** Events asked for at a time while playing, in seconds of recording. */
const AHEAD = 2;

export type PlayerStatus =
  | { kind: "loading" }
  | { kind: "locked" }
  | { kind: "failed"; text: string }
  | { kind: "ready"; duration: number };

export interface PlayerState {
  readonly status: PlayerStatus;
  /** How far seek checkpoints reach, 0 to 1. */
  readonly built: number;
  /** Where playback is, in seconds. */
  readonly time: number;
  readonly playing: boolean;
  readonly speed: number;
  readonly skipIdle: boolean;
  /** How long the last seek took to show, in ms. */
  readonly lastSeekMs: number | undefined;
  /** Seeks shown so far. */
  readonly seeks: number;
}

export class PlayerController {
  readonly #client: RecordingsClient;
  readonly #recording: RecordingView;
  readonly #makeEngine: () => PlayerEngine;
  #engine: PlayerEngine | undefined;
  #loaded: Loaded | undefined;
  #term: Terminal | undefined;
  #state: PlayerState = {
    status: { kind: "loading" },
    built: 0,
    time: 0,
    playing: false,
    speed: 1,
    skipIdle: true,
    lastSeekMs: undefined,
    seeks: 0,
  };
  readonly #listeners = new Set<() => void>();
  /** The next event to write, and the recording's clock. */
  #position = { event: 0, time: 0 };
  #seeks = 0;
  /** A seek is writing its snapshot: playback waits. */
  #seeking = false;
  #disposed = false;
  #frame: number | undefined;

  constructor(
    client: RecordingsClient,
    recording: RecordingView,
    makeEngine: () => PlayerEngine = () => new PlayerEngine(),
  ) {
    this.#client = client;
    this.#recording = recording;
    this.#makeEngine = makeEngine;
  }

  /**
   * For `useSyncExternalStore`. The last view to leave ends playback a
   * moment later, unless one comes back first, as React's StrictMode does
   * when it mounts a view twice.
   */
  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    clearTimeout(this.#leaving);
    // Loads on the first view, so a controller React made and dropped
    // (StrictMode makes two) never reads the recording.
    if (!this.#started && !this.#disposed) {
      this.#started = true;
      void this.load();
    }
    return () => {
      this.#listeners.delete(listener);
      if (this.#listeners.size === 0) {
        this.#leaving = setTimeout(() => {
          if (this.#listeners.size === 0) this.dispose();
        }, 0);
      }
    };
  };

  #leaving: ReturnType<typeof setTimeout> | undefined;
  #started = false;

  getState = (): PlayerState => this.#state;

  #set(change: Partial<PlayerState>): void {
    this.#state = { ...this.#state, ...change };
    for (const listener of this.#listeners) listener();
  }

  /** Reads the recording and its saved checkpoints into a fresh worker. */
  async load(): Promise<void> {
    this.#engine?.dispose();
    const engine = this.#makeEngine();
    this.#engine = engine;
    this.#set({ status: { kind: "loading" } });
    engine.onProgress = (share) => {
      if (this.#engine === engine) this.#set({ built: share });
    };
    engine.onBuilt = (checkpoints) => {
      this.#client.saveCheckpoints(this.#recording.id, checkpoints).catch((error: unknown) => {
        console.warn("could not keep the recording's checkpoints", error);
      });
    };
    const chunks: Uint8Array[] = [];
    try {
      const [saved, complete] = await Promise.all([
        this.#client.checkpoints(this.#recording.id).catch(() => null),
        this.#client.read(this.#recording.id, (bytes) => chunks.push(bytes)),
      ]);
      const bytes = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
      let at = 0;
      for (const chunk of chunks) {
        bytes.set(chunk, at);
        at += chunk.length;
      }
      chunks.length = 0;
      const loaded = await engine.load(bytes, saved ?? undefined, complete);
      if (this.#engine !== engine || this.#disposed) return;
      this.#loaded = loaded;
      this.#set({ status: { kind: "ready", duration: loaded.duration } });
      this.#openTerminal();
    } catch (failure) {
      if (this.#engine !== engine || this.#disposed) return;
      if (failure instanceof SessionCommandError && failure.code === "vault_locked") {
        this.#set({ status: { kind: "locked" } });
      } else {
        this.#set({ status: { kind: "failed", text: messageOf(failure) } });
      }
    }
  }

  #host: HTMLElement | undefined;

  /** Shows the terminal in `host`; `undefined` takes it away. */
  attach(host: HTMLElement | undefined): void {
    this.#host = host;
    this.#openTerminal();
  }

  #openTerminal(): void {
    const loaded = this.#loaded;
    const host = this.#host;
    if (!loaded || !host || this.#term) return;
    const term = new Terminal({
      cols: loaded.header.width,
      rows: loaded.header.height,
      scrollback: CHECKPOINT_SCROLLBACK,
      disableStdin: true,
      cursorBlink: false,
      allowProposedApi: true,
      fontSize: 13,
      // A recording's links open nothing: it is a record, not a session.
      linkHandler: { activate: () => {}, allowNonHttpProtocols: false },
    });
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    hardenParser(term);
    term.open(host);
    this.#term = term;
    this.#position = { event: 0, time: 0 };
    void this.seek(0);
  }

  /** Shows the screen at `seconds`, from a checkpoint the worker restores. */
  async seek(seconds: number): Promise<void> {
    const engine = this.#engine;
    const term = this.#term;
    const loaded = this.#loaded;
    if (!engine || !term || !loaded) return;
    const began = performance.now();
    this.#seeks += 1;
    const mine = this.#seeks;
    this.#set({ time: seconds });
    const frame = await engine.seek(eventAt(loaded.times, seconds));
    // A newer seek wins.
    if (mine !== this.#seeks || this.#term !== term) return;
    // Written hidden: one paint of the result, not of every step.
    this.#seeking = true;
    const element = term.element;
    if (element) element.style.visibility = "hidden";
    term.reset();
    if (term.cols !== frame.cols || term.rows !== frame.rows) term.resize(frame.cols, frame.rows);
    await new Promise<void>((resolve) => term.write(frame.snapshot, resolve));
    if (element) element.style.visibility = "";
    await new Promise((resolve) => requestAnimationFrame(resolve));
    this.#seeking = false;
    if (mine !== this.#seeks) return;
    const shown = frame.event > 0 ? (loaded.times[frame.event - 1] ?? seconds) : 0;
    this.#position = { event: frame.event, time: Math.max(seconds, shown) };
    this.#set({
      time: this.#position.time,
      lastSeekMs: Math.round(performance.now() - began),
      seeks: this.#state.seeks + 1,
    });
  }

  play(): void {
    const loaded = this.#loaded;
    if (!loaded || this.#state.playing) return;
    if (this.#position.time >= loaded.duration) {
      // From the start; playback waits for the seek's screen.
      this.#position = { event: 0, time: 0 };
      void this.seek(0);
    }
    this.#set({ playing: true });
    this.#run();
  }

  pause(): void {
    this.#set({ playing: false });
    if (this.#frame !== undefined) cancelAnimationFrame(this.#frame);
    this.#frame = undefined;
  }

  setSpeed(speed: number): void {
    this.#set({ speed });
  }

  setSkipIdle(skip: boolean): void {
    this.#set({ skipIdle: skip });
  }

  search(query: string): Promise<Hit[]> {
    return this.#engine ? this.#engine.search(query) : Promise.resolve([]);
  }

  /** Writes events as the clock reaches them, one animation frame a step. */
  #run(): void {
    let queue: PlayEvent[] = [];
    let next = 0;
    let asking = false;
    let wall = performance.now();
    const tick = () => {
      const term = this.#term;
      const engine = this.#engine;
      const loaded = this.#loaded;
      if (!this.#state.playing || !term || !engine || !loaded || this.#disposed) return;
      const now = performance.now();
      if (this.#seeking) {
        // The clock waits for the seek too.
        wall = now;
        queue = [];
        next = 0;
        this.#frame = requestAnimationFrame(tick);
        return;
      }
      let clock = this.#position.time + ((now - wall) / 1000) * this.#state.speed;
      wall = now;
      // Ask for more before the queue runs dry.
      const left = queue.length - next;
      if (!asking && left * 2 <= queue.length) {
        asking = true;
        const from = this.#position.event + left;
        engine
          .events(from, clock + AHEAD * this.#state.speed)
          .then((events) => {
            // Only if nothing moved meanwhile, as a seek does.
            if (this.#position.event + (queue.length - next) === from) {
              queue = [...queue.slice(next), ...events];
              next = 0;
            }
          })
          .catch(() => {})
          .finally(() => {
            asking = false;
          });
      }
      // A long pause plays short: jump the clock to just before the next
      // event, known from the timeline even when it is beyond what is queued.
      const upcoming = queue[next]?.time ?? loaded.times[this.#position.event];
      if (this.#state.skipIdle && upcoming !== undefined && upcoming - clock > IDLE_CAP) {
        clock = upcoming - IDLE_CAP;
      }
      let text = "";
      while (next < queue.length && (queue[next]?.time ?? Infinity) <= clock) {
        const event = queue[next];
        next += 1;
        if (!event) break;
        if (event.kind === OUTPUT) {
          text += event.data;
        } else if (event.kind === RESIZE) {
          if (text) term.write(text);
          text = "";
          const size = parseSize(event.data);
          if (size) term.resize(size.cols, size.rows);
        }
        this.#position.event += 1;
      }
      if (text) term.write(text);
      this.#position.time = Math.min(clock, loaded.duration);
      this.#set({ time: this.#position.time });
      if (this.#position.event >= loaded.count && next >= queue.length) {
        this.pause();
        return;
      }
      this.#frame = requestAnimationFrame(tick);
    };
    this.#frame = requestAnimationFrame(tick);
  }

  /** Where playback is now, in seconds; read without a render. */
  get time(): number {
    return this.#position.time;
  }

  dispose(): void {
    this.#disposed = true;
    this.pause();
    this.#engine?.dispose();
    this.#engine = undefined;
    this.#term?.dispose();
    this.#term = undefined;
    this.#listeners.clear();
  }
}
