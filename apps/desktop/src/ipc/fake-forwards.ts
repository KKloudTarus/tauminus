// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Forwarding in memory for end-to-end tests. Only the `e2e` build loads it.
// A rule named `busy` fails to start, as a port already in use would.

import type { ForwardState, ForwardsClient } from "./forwards.ts";

const key = (host: string, rule: string) => `${host}\u0000${rule}`;

export function fakeForwards(ruleLabel: (host: string, rule: string) => string): ForwardsClient {
  const states = new Map<string, ForwardState>();
  let nextPort = 41_000;
  return {
    status: () => Promise.resolve([...states.values()]),
    start: (host, rule) => {
      if (ruleLabel(host, rule) === "busy") {
        states.set(key(host, rule), {
          host,
          rule,
          port: null,
          starting: false,
          open: 0,
          resets: 0,
          total: 0,
          ended: "could not listen on 127.0.0.1:1080: address in use",
        });
        return Promise.reject(new Error("could not listen on 127.0.0.1:1080: address in use"));
      }
      const running = states.get(key(host, rule));
      if (running?.port != null) return Promise.resolve(running.port);
      const port = nextPort++;
      states.set(key(host, rule), {
        host,
        rule,
        port,
        starting: false,
        open: 0,
        total: 0,
        resets: 0,
        ended: null,
      });
      return Promise.resolve(port);
    },
    stop: (host, rule) => {
      const running = states.get(key(host, rule))?.port != null;
      states.delete(key(host, rule));
      return Promise.resolve(running);
    },
  };
}
