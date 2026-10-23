// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Stroke icons on a 16 px grid. Inline SVG: no font or sprite to load, and
// each one inherits the text color. Decorative only; the button they sit in
// carries the accessible name.

import type { ReactNode } from "react";

export type IconName =
  | "plus"
  | "x"
  | "sidebar"
  | "search"
  | "command"
  | "terminal"
  | "server"
  | "folder"
  | "folderPlus"
  | "files"
  | "pencil"
  | "key"
  | "chevron"
  | "sun"
  | "moon"
  | "monitor"
  | "plug";

const PATHS: Record<IconName, ReactNode> = {
  plus: <path d="M8 3.5v9M3.5 8h9" />,
  x: <path d="m4.5 4.5 7 7m0-7-7 7" />,
  sidebar: (
    <>
      <rect x="2" y="3" width="12" height="10" rx="2" />
      <path d="M6 3v10" />
    </>
  ),
  search: (
    <>
      <circle cx="7" cy="7" r="4" />
      <path d="m10 10 3.5 3.5" />
    </>
  ),
  command: (
    <path d="M6 6V4.5a1.5 1.5 0 1 0-1.5 1.5H6Zm0 0h4m-4 0v4m4-4V4.5A1.5 1.5 0 1 1 11.5 6H10Zm0 0v4m0 0h1.5a1.5 1.5 0 1 1-1.5 1.5V10Zm0 0H6m0 0v1.5A1.5 1.5 0 1 1 4.5 10H6Z" />
  ),
  terminal: (
    <>
      <rect x="2" y="3" width="12" height="10" rx="2" />
      <path d="m5 6.5 2 1.5-2 1.5M8.5 10H11" />
    </>
  ),
  server: (
    <>
      <rect x="2.5" y="2.5" width="11" height="4.5" rx="1.5" />
      <rect x="2.5" y="9" width="11" height="4.5" rx="1.5" />
      <path d="M5 4.75h.01M5 11.25h.01" />
    </>
  ),
  folder: (
    <path d="M2.5 5A1.5 1.5 0 0 1 4 3.5h2.4l1.4 1.5H12A1.5 1.5 0 0 1 13.5 6.5v5A1.5 1.5 0 0 1 12 13H4a1.5 1.5 0 0 1-1.5-1.5V5Z" />
  ),
  folderPlus: (
    <>
      <path d="M2.5 5A1.5 1.5 0 0 1 4 3.5h2.4l1.4 1.5H12A1.5 1.5 0 0 1 13.5 6.5v5A1.5 1.5 0 0 1 12 13H4a1.5 1.5 0 0 1-1.5-1.5V5Z" />
      <path d="M8 7v4M6 9h4" />
    </>
  ),
  files: (
    <>
      <path d="M9 2.5H5A1.5 1.5 0 0 0 3.5 4v8A1.5 1.5 0 0 0 5 13.5h6a1.5 1.5 0 0 0 1.5-1.5V6L9 2.5Z" />
      <path d="M9 2.5V6h3.5" />
    </>
  ),
  pencil: <path d="m10.5 3 2.5 2.5-7 7H3.5V10l7-7Z" />,
  key: (
    <>
      <circle cx="5.5" cy="10.5" r="2.5" />
      <path d="m7.3 8.7 5.2-5.2M10.5 5.5l1.5 1.5" />
    </>
  ),
  chevron: <path d="m6 4 4 4-4 4" />,
  sun: (
    <>
      <circle cx="8" cy="8" r="2.75" />
      <path d="M8 1.75v1.5m0 9.5v1.5M1.75 8h1.5m9.5 0h1.5M3.6 3.6l1 1m6.8 6.8 1 1m0-8.8-1 1m-6.8 6.8-1 1" />
    </>
  ),
  moon: <path d="M13 9.5A5.5 5.5 0 0 1 6.5 3a5.5 5.5 0 1 0 6.5 6.5Z" />,
  monitor: (
    <>
      <rect x="2" y="2.5" width="12" height="8.5" rx="1.5" />
      <path d="M6 13.5h4M8 11v2.5" />
    </>
  ),
  plug: <path d="M6 2.5V5m4-2.5V5M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0V5ZM8 11v2.5" />,
};

export function Icon({ name, className = "size-4" }: { name: IconName; className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      {PATHS[name]}
    </svg>
  );
}
