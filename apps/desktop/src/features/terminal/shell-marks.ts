// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { IMarker as Marker } from "@xterm/xterm";

/**
 * What a shell that marks its prompts (OSC 133) says: whether it waits for
 * a command, and where the line being typed starts. Kept by the pane
 * whether or not it records history.
 */
export class ShellMarks {
  readonly #mark: () => Marker | undefined;
  readonly #cursorX: () => number;
  #integrated = false;
  #atPrompt = false;
  /** An A came since the last command started: a B may follow it. */
  #prompted = false;
  #input: { marker: Marker | undefined; x: number } | undefined;

  constructor(mark: () => Marker | undefined, cursorX: () => number) {
    this.#mark = mark;
    this.#cursorX = cursorX;
  }

  /** `A` prompt, `B` input, `C` output, `D;n` done. */
  handle(data: string): void {
    switch (data.charAt(0)) {
      case "A": {
        // A secondary prompt (kitty's `k=s`): the command goes on, and its
        // line is not where the command starts.
        const secondary = /;k=s(?:;|$)/u.test(data);
        this.#atPrompt = secondary;
        this.#prompted = !secondary;
        if (secondary) {
          this.#input?.marker?.dispose();
          this.#input = undefined;
        }
        break;
      }
      case "B":
        // Only after a prompt's A: a B alone in a program's output (a log
        // being followed) does not start a line. Marks are a hint the host
        // writes, never a boundary.
        if (!this.#prompted) break;
        this.#prompted = false;
        this.#integrated = true;
        this.#atPrompt = true;
        this.#input?.marker?.dispose();
        this.#input = { marker: this.#mark(), x: this.#cursorX() };
        break;
      case "C":
        this.#atPrompt = false;
        this.#prompted = false;
        break;
      default:
        break;
    }
  }

  /** A line went to the shell: it runs until the next prompt's marks. */
  submitted(): void {
    this.#atPrompt = false;
    this.#prompted = false;
  }

  /** Whether the shell waits for a command; `undefined` if it marks none. */
  get atPrompt(): boolean | undefined {
    return this.#integrated ? this.#atPrompt : undefined;
  }

  /** Where the line being typed starts, while the shell waits for one. */
  get input(): { line: number; x: number } | undefined {
    const input = this.#input;
    if (!this.#integrated || !this.#atPrompt || !input?.marker || input.marker.line < 0) {
      return undefined;
    }
    return { line: input.marker.line, x: input.x };
  }

  dispose(): void {
    this.#input?.marker?.dispose();
    this.#input = undefined;
  }
}
