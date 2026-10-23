// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Runs the player's engine off the page: indexing, checkpoints, seeks, and
// search take seconds on a long recording and must not stall the UI.

// First, before xterm.js loads: see the file.
// oxlint-disable-next-line import/no-unassigned-import -- imported for its effect, in order
import "./worker-env.ts";
import { PlayerCore, encodeCheckpoints } from "./player-core.ts";
import type { Reply, Request } from "./player-protocol.ts";

let core: PlayerCore | undefined;
/** The newest search: an older one stops. */
let latestSearch = 0;
/** What the IPC takes for saved checkpoints at most, in characters. */
const MAX_SAVED = 60 * 1024 * 1024;

/** How long without a request counts as quiet, in ms. */
const QUIET_MS = 1500;
let quiet: { timer: ReturnType<typeof setTimeout>; work: () => void } | undefined;

/** Runs `work` once no request has come for {@link QUIET_MS}. */
function whenQuiet(work: () => void): void {
  if (quiet) clearTimeout(quiet.timer);
  quiet = {
    work,
    timer: setTimeout(() => {
      quiet = undefined;
      work();
    }, QUIET_MS),
  };
}

/** A request came: quiet work waits again. */
function busy(): void {
  if (quiet) whenQuiet(quiet.work);
}

function post(reply: Reply): void {
  postMessage(reply);
}

async function handle(request: Request): Promise<void> {
  switch (request.type) {
    case "load": {
      core?.stop();
      const loaded = new PlayerCore(new Uint8Array(request.bytes), request.saved);
      core = loaded;
      post({
        type: "loaded",
        id: request.id,
        header: loaded.cast.header,
        count: loaded.cast.count,
        duration: loaded.cast.duration,
        times: loaded.cast.times.slice(),
      });
      await loaded.build((share) => post({ type: "progress", share }));
      // Saved only for a recording that is done: one still being written,
      // or cut short, has more to come. Encoding megabytes of them takes a
      // while, so it waits for a quiet moment, not in front of a seek.
      if (loaded === core && loaded.builtHere && request.complete) {
        whenQuiet(() => {
          if (loaded === core) {
            const checkpoints = encodeCheckpoints(loaded.checkpoints);
            // Too many to keep: built again next time instead.
            if (checkpoints.length <= MAX_SAVED) post({ type: "built", checkpoints });
          }
        });
      }
      return;
    }
    case "seek": {
      if (!core) return;
      post({ type: "frame", id: request.id, frame: await core.seek(request.event) });
      return;
    }
    case "events": {
      if (!core) return;
      post({ type: "events", id: request.id, events: core.events(request.from, request.until) });
      return;
    }
    case "search": {
      if (!core) return;
      latestSearch = request.id;
      const hits = await core.search(request.query, 500, () => latestSearch !== request.id);
      // A newer search answers for this one.
      post({ type: "found", id: request.id, hits: hits ?? [] });
      return;
    }
  }
}

addEventListener("message", (message: MessageEvent<Request>) => {
  const request = message.data;
  busy();
  handle(request).catch((error: unknown) => {
    post({
      type: "failed",
      id: request.id,
      message: error instanceof Error ? error.message : String(error),
    });
  });
});
