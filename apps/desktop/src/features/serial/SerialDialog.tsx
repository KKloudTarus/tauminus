// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useCallback, useEffect, useId, useState } from "react";

import type {
  EncodingSpec,
  FlowSpec,
  ParitySpec,
  SerialClient,
  SerialPortsView,
  SerialSpec,
} from "@/ipc/index.ts";

const FIELD =
  "w-full rounded border border-current/20 bg-transparent px-2 py-1 outline-none focus:border-current/50";
const BUTTON = "rounded px-2 py-0.5 hover:bg-current/10 disabled:opacity-40";
/** What most consoles use: 115200 8N1, no flow control. */
const CONSOLE_BAUD = 115_200;

/** Picks a serial port and its line settings, then opens it in a tab. */
export function SerialDialog({
  client,
  onOpen,
  onClose,
}: {
  client: SerialClient;
  onOpen: (port: SerialSpec) => void;
  onClose: () => void;
}) {
  const titleId = useId();
  const [view, setView] = useState<SerialPortsView | undefined>();
  const [error, setError] = useState<string | undefined>();
  const [path, setPath] = useState("");
  const [baud, setBaud] = useState(CONSOLE_BAUD);
  const [dataBits, setDataBits] = useState(8);
  const [parity, setParity] = useState<ParitySpec>("none");
  const [stopBits, setStopBits] = useState(1);
  const [flow, setFlow] = useState<FlowSpec>("none");
  const [encoding, setEncoding] = useState<EncodingSpec>("UTF-8");
  const list = useCallback(() => {
    client.ports().then(
      (listed) => {
        setView(listed);
        setError(undefined);
        setPath((current) => current || (listed.ports[0]?.path ?? ""));
      },
      (failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)),
    );
  }, [client]);
  useEffect(list, [list]);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!path.trim()) return;
    onOpen({ path: path.trim(), baud, dataBits, parity, stopBits, flow, encoding });
  };
  const listId = useId();
  return (
    <div className="fixed inset-0 z-20 flex items-start justify-center bg-black/30 pt-[10vh]">
      <dialog
        open
        aria-labelledby={titleId}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        className="static m-0 w-[min(480px,92vw)] rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl"
      >
        <h2 id={titleId} className="mb-3 text-base font-medium">
          Open a serial port
        </h2>
        <form onSubmit={submit} className="flex flex-col gap-2">
          <label className="flex flex-col gap-1">
            Port
            <div className="flex gap-2">
              <input
                className={`${FIELD} font-mono`}
                value={path}
                list={listId}
                required
                spellCheck={false}
                placeholder={view?.ports.length === 0 ? "None found; type a path" : "/dev/ttyUSB0"}
                onChange={(e) => setPath(e.target.value)}
              />
              <button type="button" className={BUTTON} onClick={list}>
                Refresh
              </button>
            </div>
            <datalist id={listId}>
              {view?.ports.map((port) => (
                <option key={port.path} value={port.path}>
                  {port.description || port.path}
                </option>
              ))}
            </datalist>
          </label>
          {error && (
            <p role="alert" className="text-xs text-red-600 dark:text-red-400">
              {error}
            </p>
          )}
          {view && view.ports.length > 0 && (
            <ul aria-label="Attached ports" className="text-xs opacity-70">
              {view.ports.map((port) => (
                <li key={port.path}>
                  <span className="font-mono">{port.path}</span>
                  {port.description && ` · ${port.description}`}
                </li>
              ))}
            </ul>
          )}
          <div className="grid grid-cols-2 gap-2">
            <label className="flex flex-col gap-1">
              Speed (baud)
              <select
                className={FIELD}
                value={baud}
                onChange={(e) => setBaud(Number(e.target.value))}
              >
                {(view?.baudRates ?? [CONSOLE_BAUD]).map((rate) => (
                  <option key={rate} value={rate}>
                    {rate}
                  </option>
                ))}
              </select>
            </label>
            <label className="flex flex-col gap-1">
              Data bits
              <select
                className={FIELD}
                value={dataBits}
                onChange={(e) => setDataBits(Number(e.target.value))}
              >
                {[8, 7, 6, 5].map((bits) => (
                  <option key={bits} value={bits}>
                    {bits}
                  </option>
                ))}
              </select>
            </label>
            <label className="flex flex-col gap-1">
              Parity
              <select
                className={FIELD}
                value={parity}
                onChange={(e) => setParity(parityChoice(e.target.value))}
              >
                <option value="none">None</option>
                <option value="odd">Odd</option>
                <option value="even">Even</option>
              </select>
            </label>
            <label className="flex flex-col gap-1">
              Stop bits
              <select
                className={FIELD}
                value={stopBits}
                onChange={(e) => setStopBits(Number(e.target.value))}
              >
                <option value={1}>1</option>
                <option value={2}>2</option>
              </select>
            </label>
            <label className="flex flex-col gap-1">
              Flow control
              <select
                className={FIELD}
                value={flow}
                onChange={(e) => setFlow(flowChoice(e.target.value))}
              >
                <option value="none">None</option>
                <option value="software">XON/XOFF</option>
                <option value="hardware">RTS/CTS</option>
              </select>
            </label>
            <label className="flex flex-col gap-1">
              Character set
              <select
                className={FIELD}
                value={encoding}
                onChange={(e) => setEncoding(encodingChoice(e.target.value))}
              >
                {ENCODINGS.map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <div className="mt-2 flex justify-end gap-2">
            <button type="button" className={BUTTON} onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className={BUTTON} disabled={!path.trim()}>
              Open
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}

const ENCODINGS: readonly EncodingSpec[] = [
  "UTF-8",
  "windows-1252",
  "ISO-8859-2",
  "windows-1251",
  "KOI8-R",
  "Shift_JIS",
  "EUC-JP",
  "gb18030",
  "Big5",
  "EUC-KR",
];

function encodingChoice(value: string): EncodingSpec {
  return ENCODINGS.find((name) => name === value) ?? "UTF-8";
}

function parityChoice(value: string): ParitySpec {
  return value === "odd" || value === "even" ? value : "none";
}

function flowChoice(value: string): FlowSpec {
  return value === "software" || value === "hardware" ? value : "none";
}
