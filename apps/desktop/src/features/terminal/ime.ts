// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

const COMPOSED = new Set(["insertFromComposition", "insertCompositionText"]);

/**
 * Starts the compositions an input method commits without announcing.
 *
 * WebKitGTK with IBus, as with ibus-unikey typing Telex, sends `beforeinput`
 * and `input` of type `insertFromComposition`, then `compositionend`, but no
 * `compositionstart`. xterm.js 6 records where a composition begins in
 * `compositionstart`; without it the start stays at 0, and after each commit
 * xterm.js sends the tail of the buffer again ("tiếng " arrives as
 * "tiếng ng "). Dispatching the missing event just before the text lands
 * gives xterm.js the right start. Browsers that announce compositions are
 * left alone.
 *
 * Returns a function that removes the listeners.
 */
export function startUnannouncedCompositions(textarea: HTMLTextAreaElement): () => void {
  let composing = false;
  const started = () => {
    composing = true;
  };
  const ended = () => {
    composing = false;
  };
  const beforeInput = (event: InputEvent) => {
    if (composing || !COMPOSED.has(event.inputType)) return;
    textarea.dispatchEvent(new CompositionEvent("compositionstart", { data: "" }));
  };
  textarea.addEventListener("compositionstart", started);
  textarea.addEventListener("compositionend", ended);
  textarea.addEventListener("beforeinput", beforeInput);
  return () => {
    textarea.removeEventListener("compositionstart", started);
    textarea.removeEventListener("compositionend", ended);
    textarea.removeEventListener("beforeinput", beforeInput);
  };
}
