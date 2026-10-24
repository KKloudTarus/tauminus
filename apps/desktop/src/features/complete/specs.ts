// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { SpecLoader } from "./match.ts";
import type { SpecNode } from "./spec.ts";

// One chunk per spec, fetched the first time its command is completed.
const files = import.meta.glob<SpecNode>("./specs/*.json", { import: "default" });

/** The commands with a spec, sorted. */
export const COMMANDS: readonly string[] = Object.keys(files)
  .map((file) => file.slice("./specs/".length, -".json".length))
  .toSorted();

const loaded = new Map<string, Promise<SpecNode | undefined>>();

/** A command's spec, loaded once; `undefined` for a command without one. */
export const loadSpec: SpecLoader = (command) => {
  const file = files[`./specs/${command}.json`];
  if (!file) return Promise.resolve(undefined);
  let spec = loaded.get(command);
  if (!spec) {
    spec = file().catch(() => {
      loaded.delete(command);
      return undefined;
    });
    loaded.set(command, spec);
  }
  return spec;
};
