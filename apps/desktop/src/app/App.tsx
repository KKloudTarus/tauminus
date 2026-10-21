// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { TerminalController } from "@/features/terminal/controller.ts";
import { TerminalView } from "@/features/terminal/TerminalView.tsx";

interface AppProps {
  terminal: TerminalController;
}

export function App({ terminal }: AppProps) {
  return (
    <main className="flex h-screen flex-col bg-background text-foreground">
      <TerminalView controller={terminal} className="min-h-0 flex-1" />
    </main>
  );
}
