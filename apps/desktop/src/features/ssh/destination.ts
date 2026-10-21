// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { EndpointSpec } from "@/ipc/index.ts";

export type Parsed<T> = { ok: true; value: T } | { ok: false; error: string };

const USER = /^[A-Za-z0-9._@-]{1,64}$/;
const HOST_LABEL = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/;
const IPV6 = /^[0-9A-Fa-f:.]+$/;

/**
 * Parses an `ssh` destination: `host`, `user@host`, `user@host:port`,
 * `user@[2001:db8::1]:port`, or `ssh://user@host:port`. An omitted user is
 * `""`, which the backend reads as the local user name. The backend validates
 * again; this check exists to give an error while the user types. O(n).
 */
export function parseDestination(input: string, defaultPort = 22): Parsed<EndpointSpec> {
  let text = input.trim();
  if (text.startsWith("ssh://")) text = text.slice("ssh://".length).replace(/\/$/, "");
  if (text === "") return fail("Enter a host, for example user@example.com");

  // The last @ separates the user: user names may contain @, host names cannot.
  const at = text.lastIndexOf("@");
  const user = at >= 0 ? text.slice(0, at) : "";
  const rest = at >= 0 ? text.slice(at + 1) : text;
  if (at >= 0 && !(USER.test(user) && !user.startsWith("-"))) {
    return fail(`"${user}" is not a valid user name`);
  }

  let host: string;
  let portText: string | undefined;
  if (rest.startsWith("[")) {
    const close = rest.indexOf("]");
    if (close < 0) return fail("Missing ] after the IPv6 address");
    host = rest.slice(1, close);
    const after = rest.slice(close + 1);
    if (after !== "" && !after.startsWith(":")) return fail("Expected :port after ]");
    portText = after === "" ? undefined : after.slice(1);
    if (!IPV6.test(host) || !host.includes(":")) return fail(`"${host}" is not an IPv6 address`);
  } else {
    const colons = rest.split(":").length - 1;
    if (colons > 1) {
      // A bare IPv6 address, as ssh accepts it, with no port.
      host = rest;
      if (!IPV6.test(host)) return fail(`"${host}" is not a valid host`);
    } else {
      const colon = rest.indexOf(":");
      host = colon >= 0 ? rest.slice(0, colon) : rest;
      portText = colon >= 0 ? rest.slice(colon + 1) : undefined;
      if (!validHostName(host)) return fail(`"${host}" is not a valid host name`);
    }
  }

  const port = portText === undefined ? defaultPort : parsePort(portText);
  if (port === undefined) return fail(`"${portText ?? ""}" is not a port between 1 and 65535`);
  return { ok: true, value: { user, host, port } };
}

/** Parses a comma-separated jump list, as `ssh -J` takes it. Empty input is no jumps. */
export function parseJumps(input: string): Parsed<EndpointSpec[]> {
  const parts = input
    .split(",")
    .map((part) => part.trim())
    .filter((part) => part !== "");
  const jumps: EndpointSpec[] = [];
  for (const part of parts) {
    const parsed = parseDestination(part);
    if (!parsed.ok) return parsed;
    jumps.push(parsed.value);
  }
  return { ok: true, value: jumps };
}

/** How a destination is shown: `user@host`, with the port when it is not 22. */
export function formatEndpoint({ user, host, port }: EndpointSpec): string {
  const shownHost = host.includes(":") ? `[${host}]` : host;
  const base = user ? `${user}@${shownHost}` : shownHost;
  return port === 22 ? base : `${base}:${port}`;
}

function validHostName(host: string): boolean {
  return (
    host.length > 0 &&
    host.length <= 253 &&
    host.split(".").every((label) => HOST_LABEL.test(label))
  );
}

function parsePort(text: string): number | undefined {
  if (!/^\d{1,5}$/.test(text)) return undefined;
  const port = Number(text);
  return port >= 1 && port <= 65_535 ? port : undefined;
}

function fail(error: string): { ok: false; error: string } {
  return { ok: false, error };
}
