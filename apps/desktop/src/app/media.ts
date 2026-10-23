// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useSyncExternalStore } from "react";

/** Below this width the host list becomes a drawer and dialogs become sheets. */
export const NARROW = "(max-width: 639px)";

/** One MediaQueryList per query, so a render reads `matches` without parsing. */
const lists = new Map<string, MediaQueryList>();

function list(query: string): MediaQueryList | undefined {
  if (typeof window === "undefined" || !window.matchMedia) return undefined;
  let found = lists.get(query);
  if (!found) {
    found = window.matchMedia(query);
    lists.set(query, found);
  }
  return found;
}

/** Whether `query` matches now; re-renders only when that flips. O(1) per render. */
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback(
    (listener: () => void) => {
      const media = list(query);
      media?.addEventListener("change", listener);
      return () => media?.removeEventListener("change", listener);
    },
    [query],
  );
  return useSyncExternalStore(
    subscribe,
    () => list(query)?.matches ?? false,
    () => false,
  );
}
