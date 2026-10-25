// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { useEffect, useId, useRef, useState } from "react";

import { Icon } from "@/app/icons.tsx";
import type { ImportKind } from "@/ipc/index.ts";

/** The menu's entries: `~/.ssh/config`, then every other client, in order. */
/** Where an import comes from: a client's file, `~/.ssh/config`, or a cloud account. */
export type ImportSource = ImportKind | "sshConfig" | "cloud";

export const IMPORT_ENTRIES: readonly { kind: ImportSource; label: string }[] = [
  { kind: "sshConfig", label: "From ~/.ssh/config" },
  { kind: "putty", label: "From PuTTY on this computer" },
  { kind: "puttyExport", label: "From a PuTTY .reg export…" },
  { kind: "mobaXterm", label: "From MobaXterm (.ini, .mxtsessions)…" },
  { kind: "secureCrt", label: "From SecureCRT's session folder…" },
  { kind: "csv", label: "From a CSV file…" },
  { kind: "cloud", label: "From a cloud account…" },
];

interface ImportMenuProps {
  onImport: (kind: ImportSource) => void;
}

/**
 * A button that opens the list of places hosts can come from. Arrow keys
 * move through it, Escape or a click outside closes it, and focus goes
 * back to the button.
 */
export function ImportMenu({ onImport }: ImportMenuProps) {
  const [open, setOpen] = useState(false);
  const menuId = useId();
  const button = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return undefined;
    menu.current?.querySelector<HTMLButtonElement>("[role=menuitem]")?.focus();
    const outside = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (!menu.current?.contains(target) && !button.current?.contains(target)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);

  const close = () => {
    setOpen(false);
    button.current?.focus();
  };

  return (
    <div className="relative">
      <button
        ref={button}
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-label="Import hosts"
        title="Import hosts"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        className="icon-btn"
      >
        <Icon name="import" />
      </button>
      {open && (
        <div
          ref={menu}
          id={menuId}
          role="menu"
          tabIndex={-1}
          aria-label="Import hosts"
          onKeyDown={(event) => {
            const items = [
              ...(menu.current?.querySelectorAll<HTMLButtonElement>("[role=menuitem]") ?? []),
            ];
            const focused = document.activeElement;
            const at = focused instanceof HTMLButtonElement ? items.indexOf(focused) : -1;
            if (event.key === "Escape") {
              event.preventDefault();
              close();
            } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              const step = event.key === "ArrowDown" ? 1 : -1;
              items[(at + step + items.length) % items.length]?.focus();
            }
          }}
          className="panel absolute top-full left-0 z-20 mt-1 w-72 max-w-[calc(100vw-1rem)] py-1 text-sm"
        >
          {IMPORT_ENTRIES.map((entry) => (
            <button
              key={entry.kind}
              type="button"
              role="menuitem"
              onClick={() => {
                close();
                onImport(entry.kind);
              }}
              className="block w-full px-3 py-1.5 text-left hover:bg-current/10 focus-visible:bg-current/10"
            >
              {entry.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
