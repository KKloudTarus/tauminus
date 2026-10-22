// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/** The class that holds the input textarea still; styles.css gives it effect. */
export const PINNED_CLASS = "tauminus-pinned-input";

/**
 * Holds xterm.js's input textarea still while its pane takes bulk output.
 *
 * xterm.js moves the hidden textarea to the cursor on every cursor move, so
 * an input method can put its window there. When that textarea has focus,
 * WebKitGTK answers each move with editor and caret updates, and a focused
 * pane running `yes` beside two others dropped 6 to 15% of frames against
 * 2 to 3% with the textarea held still. The pin applies only while output
 * arrives in chunks larger than `bulkBytes`, which keystroke echoes never
 * are, and lets go `quietMs` after the last one, or at once when the user
 * starts composing, so the input method's window still lands on the cursor.
 * The textarea's inline position stays current underneath, so it is right
 * the moment the pin goes.
 *
 * `output` is O(1); one timer runs while pinned.
 */
export class TextareaPin {
  readonly #element: HTMLElement;
  readonly #textarea: HTMLTextAreaElement;
  readonly #bulkBytes: number;
  readonly #quietMs: number;
  readonly #now: () => number;
  readonly #setTimer: (task: () => void, ms: number) => () => void;
  #lastBulk = 0;
  #composing = false;
  #cancelTimer: (() => void) | undefined;

  constructor(
    element: HTMLElement,
    textarea: HTMLTextAreaElement,
    options: {
      bulkBytes?: number;
      quietMs?: number;
      now?: () => number;
      setTimer?: (task: () => void, ms: number) => () => void;
    } = {},
  ) {
    this.#element = element;
    this.#textarea = textarea;
    this.#bulkBytes = options.bulkBytes ?? 4096;
    this.#quietMs = options.quietMs ?? 150;
    this.#now = options.now ?? (() => performance.now());
    this.#setTimer =
      options.setTimer ??
      ((task, ms) => {
        const timer = setTimeout(task, ms);
        return () => clearTimeout(timer);
      });
    textarea.addEventListener("compositionstart", this.#startComposing);
    textarea.addEventListener("compositionend", this.#endComposing);
  }

  get pinned(): boolean {
    return this.#cancelTimer !== undefined;
  }

  /** Records a chunk of output the terminal is about to parse. */
  output(bytes: number): void {
    if (bytes <= this.#bulkBytes || this.#composing) return;
    this.#lastBulk = this.#now();
    if (this.pinned) return;
    this.#element.classList.add(PINNED_CLASS);
    this.#arm(this.#quietMs);
  }

  dispose(): void {
    this.#release();
    this.#textarea.removeEventListener("compositionstart", this.#startComposing);
    this.#textarea.removeEventListener("compositionend", this.#endComposing);
  }

  #arm(ms: number): void {
    this.#cancelTimer = this.#setTimer(() => {
      const quiet = this.#now() - this.#lastBulk;
      if (quiet >= this.#quietMs) {
        this.#cancelTimer = undefined;
        this.#element.classList.remove(PINNED_CLASS);
      } else {
        this.#arm(this.#quietMs - quiet);
      }
    }, ms);
  }

  #release(): void {
    this.#cancelTimer?.();
    this.#cancelTimer = undefined;
    this.#element.classList.remove(PINNED_CLASS);
  }

  readonly #startComposing = () => {
    this.#composing = true;
    this.#release();
  };

  readonly #endComposing = () => {
    this.#composing = false;
  };
}
