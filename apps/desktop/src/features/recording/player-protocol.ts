// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Messages between the player and its worker.

import type { CastHeader } from "./cast.ts";
import type { Frame, Hit, PlayEvent } from "./player-core.ts";

export type Request =
  /** `bytes` is transferred, not copied. `complete`: the recording ended cleanly. */
  | { type: "load"; id: number; bytes: ArrayBuffer; saved: string | undefined; complete: boolean }
  | { type: "seek"; id: number; event: number }
  | { type: "events"; id: number; from: number; until: number }
  | { type: "search"; id: number; query: string };

export type Reply =
  | {
      type: "loaded";
      id: number;
      header: CastHeader;
      count: number;
      duration: number;
      /** Each event's time, for the timeline; 8 bytes an event. */
      times: Float64Array;
    }
  | { type: "progress"; share: number }
  /** Checkpoints built here, to save for next time. */
  | { type: "built"; checkpoints: string }
  | { type: "frame"; id: number; frame: Frame }
  | { type: "events"; id: number; events: PlayEvent[] }
  | { type: "found"; id: number; hits: Hit[] }
  | { type: "failed"; id: number; message: string };
