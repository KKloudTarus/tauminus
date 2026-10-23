// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FlowSpec,
  type ParitySpec,
  type SerialPortView,
  type SerialPortsView,
  type SerialSpec,
  commands,
} from "./bindings.ts";

export type { FlowSpec, ParitySpec, SerialPortView, SerialPortsView, SerialSpec };

/** The serial ports attached now. Opening one is a session, through `SessionClient`. */
export interface SerialClient {
  ports(): Promise<SerialPortsView>;
}

export const tauriSerial: SerialClient = {
  ports: async () => {
    const result = await commands.serialPorts();
    if (result.status === "error") throw new Error(result.error.message);
    return result.data;
  },
};

/** How a console writes a port's settings: `/dev/ttyUSB0 115200 8N1`. */
export function describePort(port: SerialSpec): string {
  const parity = port.parity === "none" ? "N" : port.parity === "odd" ? "O" : "E";
  return `${port.path} ${port.baud} ${port.dataBits}${parity}${port.stopBits}`;
}
