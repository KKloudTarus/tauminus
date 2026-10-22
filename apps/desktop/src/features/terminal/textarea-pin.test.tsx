// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { afterEach, describe, expect, it } from "vitest";

import { PINNED_CLASS, TextareaPin } from "./textarea-pin.ts";

const cleanup: Array<() => void> = [];

afterEach(() => {
  for (const undo of cleanup.splice(0)) undo();
});

function setup() {
  const element = document.createElement("div");
  const textarea = document.createElement("textarea");
  element.append(textarea);
  document.body.append(element);
  const clock = { now: 0 };
  const timers: Array<{ at: number; task: () => void; live: boolean }> = [];
  const pin = new TextareaPin(element, textarea, {
    bulkBytes: 100,
    quietMs: 50,
    now: () => clock.now,
    setTimer: (task, ms) => {
      const timer = { at: clock.now + ms, task, live: true };
      timers.push(timer);
      return () => {
        timer.live = false;
      };
    },
  });
  const advance = (ms: number) => {
    clock.now += ms;
    for (const timer of timers.splice(0)) {
      if (timer.live && timer.at <= clock.now) timer.task();
      else if (timer.live) timers.push(timer);
    }
  };
  const pinned = () => element.classList.contains(PINNED_CLASS);
  cleanup.push(() => {
    pin.dispose();
    element.remove();
  });
  return { pin, textarea, advance, pinned };
}

describe("TextareaPin", () => {
  it("pins on bulk output and lets go once output goes quiet", () => {
    const { pin, advance, pinned } = setup();
    pin.output(64 * 1024);
    expect(pinned()).toBe(true);
    advance(30);
    pin.output(64 * 1024);
    advance(30);
    // 30 ms after the last chunk: still pinned.
    expect(pinned()).toBe(true);
    advance(25);
    expect(pinned()).toBe(false);
  });

  it("leaves keystroke echoes alone", () => {
    const { pin, pinned } = setup();
    pin.output(1);
    pin.output(100);
    expect(pinned()).toBe(false);
  });

  it("lets go when the user starts composing and stays off until it ends", () => {
    const { pin, textarea, pinned } = setup();
    pin.output(1000);
    textarea.dispatchEvent(new CompositionEvent("compositionstart"));
    expect(pinned()).toBe(false);
    pin.output(1000);
    expect(pinned()).toBe(false);
    textarea.dispatchEvent(new CompositionEvent("compositionend"));
    pin.output(1000);
    expect(pinned()).toBe(true);
  });

  it("lets go on dispose", () => {
    const { pin, pinned } = setup();
    pin.output(1000);
    pin.dispose();
    expect(pinned()).toBe(false);
  });
});
