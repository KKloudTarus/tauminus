// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Serial ports for end-to-end tests. Only the `e2e` build loads it.

import type { SerialClient } from "./serial.ts";

export function fakeSerial(): SerialClient {
  return {
    ports: () =>
      Promise.resolve({
        ports: [
          { path: "/dev/ttyUSB0", description: "USB: FTDI FT232R" },
          { path: "/dev/ttyS0", description: "" },
        ],
        baudRates: [9600, 19_200, 38_400, 57_600, 115_200],
      }),
  };
}
