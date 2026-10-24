// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { messageOf } from "@/features/hosts/hosts-store.ts";
import {
  type AnswerSpec,
  type ForwardState,
  type ForwardsClient,
  type PromptKind,
  SessionCommandError,
} from "@/ipc/index.ts";

/** A question a start raised, waiting for the user. */
export interface PendingPrompt {
  readonly prompt: PromptKind;
  readonly answer: (answer: AnswerSpec) => void;
}

export interface ForwardsState {
  /** By `host` and `rule`; see {@link ruleKey}. */
  readonly running: ReadonlyMap<string, ForwardState>;
  /** Rules this window is starting. */
  readonly busy: ReadonlySet<string>;
  /** The last start or stop that failed, by rule. */
  readonly errors: ReadonlyMap<string, string>;
  /** The question shown now; others wait behind it. */
  readonly prompt: PendingPrompt | undefined;
}

export function ruleKey(host: string, rule: string): string {
  return `${host}\u0000${rule}`;
}

const CANCEL: AnswerSpec = { kind: "cancel" };

/**
 * Forwarding as the UI sees it, outside React. Status comes from the
 * backend; while something watches, it is read once a second, which costs
 * one call listing at most a few hundred rules.
 */
export class ForwardsStore {
  readonly #client: ForwardsClient;
  readonly #listeners = new Set<() => void>();
  #state: ForwardsState = {
    running: new Map(),
    busy: new Set(),
    errors: new Map(),
    prompt: undefined,
  };
  /** Questions in the order they came; the first is shown. */
  #prompts: PendingPrompt[] = [];
  #watchers = 0;
  #timer: ReturnType<typeof setInterval> | undefined;
  /** Asks to unlock the vault, then runs `retry`. */
  readonly #unlock: (retry: () => void) => void;

  constructor(client: ForwardsClient, unlock: (retry: () => void) => void) {
    this.#client = client;
    this.#unlock = unlock;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };

  getSnapshot = (): ForwardsState => this.#state;

  /** Reads status once a second until the returned function is called. */
  watch(): () => void {
    this.#watchers += 1;
    if (this.#watchers === 1) {
      void this.refresh();
      this.#timer = setInterval(() => void this.refresh(), 1000);
    }
    let done = false;
    return () => {
      if (done) return;
      done = true;
      this.#watchers -= 1;
      if (this.#watchers === 0 && this.#timer !== undefined) {
        clearInterval(this.#timer);
        this.#timer = undefined;
      }
    };
  }

  async refresh(): Promise<void> {
    try {
      const states = await this.#client.status();
      // Entries that did not change keep their object, so rows that read
      // them skip rendering; nothing changed means no update at all. O(S).
      const before = this.#state.running;
      let changed = states.length !== before.size;
      const running = new Map<string, ForwardState>();
      for (const state of states) {
        const key = ruleKey(state.host, state.rule);
        const old = before.get(key);
        const same =
          old !== undefined &&
          old.port === state.port &&
          old.open === state.open &&
          old.total === state.total &&
          old.resets === state.resets &&
          old.starting === state.starting &&
          old.ended === state.ended;
        if (!same) changed = true;
        running.set(key, same ? old : state);
      }
      if (changed) this.#set({ ...this.#state, running });
    } catch (error) {
      console.warn("forwarding status failed", error);
    }
  }

  /** True while the rule listens. */
  isRunning(host: string, rule: string): boolean {
    const state = this.#state.running.get(ruleKey(host, rule));
    return state?.port != null;
  }

  /** True while a start, from here or from the host connecting, is under way. */
  isStarting(host: string, rule: string): boolean {
    const key = ruleKey(host, rule);
    return this.#state.busy.has(key) || this.#state.running.get(key)?.starting === true;
  }

  /** Starts a stopped rule, or stops one that runs or is starting. */
  toggle(host: string, rule: string): Promise<void> {
    return this.isRunning(host, rule) || this.isStarting(host, rule)
      ? this.stop(host, rule)
      : this.start(host, rule);
  }

  async start(host: string, rule: string): Promise<void> {
    const key = ruleKey(host, rule);
    if (this.#state.busy.has(key)) return;
    this.#mark(key, true);
    const mine = new Set<PendingPrompt>();
    try {
      await this.#client.start(host, rule, (prompt, withdrawn) =>
        this.#ask(prompt, mine, withdrawn),
      );
      this.#setError(key, undefined);
    } catch (error) {
      if (error instanceof SessionCommandError && error.code === "vault_locked") {
        this.#unlock(() => void this.start(host, rule));
      } else if (!(error instanceof SessionCommandError && error.code === "cancelled")) {
        this.#setError(key, messageOf(error));
      }
    } finally {
      this.#mark(key, false);
      // Its questions still open answer nothing now; others' stay.
      for (const pending of mine) pending.answer(CANCEL);
      await this.refresh();
    }
  }

  /** Stops a rule, including one still starting: that start then ends as cancelled. */
  async stop(host: string, rule: string): Promise<void> {
    const key = ruleKey(host, rule);
    try {
      await this.#client.stop(host, rule);
      this.#setError(key, undefined);
    } catch (error) {
      this.#setError(key, messageOf(error));
    } finally {
      await this.refresh();
    }
  }

  /** Shows questions one at a time, in the order they came. */
  #ask(prompt: PromptKind, mine: Set<PendingPrompt>, withdrawn?: AbortSignal): Promise<AnswerSpec> {
    return new Promise((resolve) => {
      const pending: PendingPrompt = {
        prompt,
        answer: (reply) => {
          if (!mine.delete(pending)) return;
          this.#prompts = this.#prompts.filter((p) => p !== pending);
          this.#set({ ...this.#state, prompt: this.#prompts[0] });
          resolve(reply);
        },
      };
      // No longer asked, as when a security key answered: it goes away.
      withdrawn?.addEventListener("abort", () => pending.answer(CANCEL), { once: true });
      mine.add(pending);
      this.#prompts = [...this.#prompts, pending];
      this.#set({ ...this.#state, prompt: this.#prompts[0] });
    });
  }

  #mark(key: string, busy: boolean): void {
    const next = new Set(this.#state.busy);
    if (busy) next.add(key);
    else next.delete(key);
    this.#set({ ...this.#state, busy: next });
  }

  #setError(key: string, message: string | undefined): void {
    if (message === undefined && !this.#state.errors.has(key)) return;
    const next = new Map(this.#state.errors);
    if (message === undefined) next.delete(key);
    else next.set(key, message);
    this.#set({ ...this.#state, errors: next });
  }

  #set(state: ForwardsState): void {
    this.#state = state;
    for (const listener of this.#listeners) listener();
  }
}
