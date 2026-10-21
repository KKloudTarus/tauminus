// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { afterEach, describe, expect, it } from "vitest";

import { startUnannouncedCompositions } from "./ime.ts";

const textareas: HTMLTextAreaElement[] = [];

function setup() {
  const textarea = document.createElement("textarea");
  document.body.append(textarea);
  textareas.push(textarea);
  const starts: string[] = [];
  textarea.addEventListener("compositionstart", () => starts.push(textarea.value));
  const stop = startUnannouncedCompositions(textarea);
  return { textarea, starts, stop };
}

/**
 * An input event as WebKit sends it. Chromium blanks input types it does not
 * implement, and `insertFromComposition` is one, so it is set by hand.
 */
function composed(data: string, inputType = "insertFromComposition") {
  const event = new InputEvent("beforeinput", { data, bubbles: true });
  Object.defineProperty(event, "inputType", { value: inputType });
  return event;
}

afterEach(() => {
  for (const textarea of textareas.splice(0)) textarea.remove();
});

describe("startUnannouncedCompositions", () => {
  it("starts a composition the input method did not announce, before its text lands", () => {
    const { textarea, starts } = setup();
    textarea.value = "ab ";
    textarea.dispatchEvent(composed("tiếng "));
    expect(starts).toEqual(["ab "]);
    textarea.dispatchEvent(new CompositionEvent("compositionend", { data: "tiếng " }));
    textarea.dispatchEvent(composed("Việt"));
    expect(starts).toEqual(["ab ", "ab "]);
  });

  it("leaves announced compositions alone", () => {
    const { textarea, starts } = setup();
    textarea.dispatchEvent(new CompositionEvent("compositionstart", { data: "" }));
    textarea.dispatchEvent(composed("t", "insertCompositionText"));
    expect(starts).toEqual([""]);
  });

  it("ignores ordinary typing and stops when asked", () => {
    const { textarea, starts, stop } = setup();
    textarea.dispatchEvent(composed("a", "insertText"));
    stop();
    textarea.dispatchEvent(composed("x"));
    expect(starts).toEqual([]);
  });
});
