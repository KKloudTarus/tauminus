// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The v0.1 benchmark, run by the app itself on every CI platform.
 *
 * Built in only when `VITE_TAUMINUS_BENCH` is "1"; `main.tsx` imports this
 * module behind that flag, so a normal build contains none of it. It splits
 * the first tab into four panes and runs `cargo xtask flood` in them, the
 * same output on every OS:
 *
 * 1. `flood text 500` in one pane, about 500 MiB of 120-column lines;
 * 2. `flood yes 60` in three panes at once, three rounds 30 s apart, so the
 *    memory samples show whether the webview keeps growing run after run.
 *
 * Meanwhile it counts animation frames and, every 250 ms, types one key into
 * the quiet fourth pane with `terminal.input` and times it until the echo is
 * on screen: the shell round trip, the output path and the paint, without
 * the OS keyboard path. The results go out through `flood report`, which
 * writes them where `TAUMINUS_BENCH_OUT` says. Memory is sampled from
 * outside, by the CI script, against the time stamps in the report.
 */

import type { AppShell } from "@/app/shell.ts";
import type { TerminalController } from "@/features/terminal/controller.ts";
import { paneIds } from "@/features/workspace/layout.ts";

import { FrameCounter, type Summary, summarize, toHex } from "./stats.ts";

interface ScenarioResult {
  name: string;
  startedAt: number;
  endedAt: number;
  frames: number;
  droppedShare: number;
  maxGapMs: number;
  latencyMs: Summary;
  completed: boolean;
}

const epoch = () => performance.timeOrigin + performance.now();
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** The last `rows` lines of a pane, newest last. */
function tail(controller: TerminalController, rows = 6): string {
  const buffer = controller.terminal.buffer.active;
  const end = buffer.baseY + buffer.cursorY;
  const lines: string[] = [];
  for (let y = Math.max(0, end - rows); y <= end; y += 1) {
    lines.push(buffer.getLine(y)?.translateToString(true) ?? "");
  }
  return lines.join("\n");
}

async function waitFor(check: () => boolean, timeoutMs: number): Promise<boolean> {
  const until = performance.now() + timeoutMs;
  while (performance.now() < until) {
    if (check()) return true;
    // oxlint-disable-next-line no-await-in-loop -- polls until the condition holds
    await sleep(100);
  }
  return check();
}

/** Types one key and times it until the echo is parsed and painted. */
function probe(controller: TerminalController): Promise<number> {
  const { terminal } = controller;
  return new Promise((resolve) => {
    const started = performance.now();
    let parsed = false;
    const finish = (value: number) => {
      written.dispose();
      rendered.dispose();
      clearTimeout(timer);
      resolve(value);
    };
    const written = terminal.onWriteParsed(() => {
      parsed = true;
    });
    const rendered = terminal.onRender(() => {
      if (parsed) finish(performance.now() - started);
    });
    const timer = setTimeout(() => finish(Number.NaN), 2000);
    terminal.input("x");
  });
}

function type(controller: TerminalController, line: string): void {
  controller.terminal.input(`${line}\r`);
}

async function scenario(
  name: string,
  quiet: TerminalController,
  start: () => void,
  done: () => boolean,
  timeoutMs: number,
): Promise<ScenarioResult> {
  const counter = new FrameCounter();
  let running = true;
  const frame = (at: number) => {
    if (!running) return;
    counter.tick(at);
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
  const startedAt = epoch();
  start();
  const latencies: number[] = [];
  const until = performance.now() + timeoutMs;
  let completed = false;
  while (performance.now() < until) {
    // oxlint-disable-next-line no-await-in-loop -- one probe at a time, 250 ms apart
    latencies.push(await probe(quiet));
    if (done()) {
      completed = true;
      break;
    }
    // oxlint-disable-next-line no-await-in-loop -- paces the probes
    await sleep(250);
  }
  running = false;
  // Ctrl+U clears the probe keys from the quiet shell's line.
  quiet.terminal.input("\u0015");
  return {
    name,
    startedAt,
    endedAt: epoch(),
    frames: counter.frames,
    droppedShare: counter.droppedShare,
    maxGapMs: counter.maxGapMs,
    latencyMs: summarize(latencies),
    completed,
  };
}

export async function runBenchmark(shell: AppShell): Promise<void> {
  const workspace = shell.workspace;
  const first = (): TerminalController | undefined => {
    const tab = workspace.activeTab;
    return tab ? workspace.terminal(tab.focused) : undefined;
  };
  let firstOutputAt = Number.NaN;
  await waitFor(() => first() !== undefined, 10_000);
  const firstPane = first()!;
  const stamp = firstPane.terminal.onWriteParsed(() => {
    firstOutputAt = epoch();
    stamp.dispose();
  });
  await waitFor(() => !Number.isNaN(firstOutputAt), 30_000);
  // Idle time for the CI script's memory sample.
  await sleep(30_000);
  const idleUntil = epoch();

  shell.run("pane.splitRight");
  shell.run("pane.splitDown");
  const tab = workspace.activeTab!;
  workspace.focusPane(paneIds(tab.root)[0]!);
  shell.run("pane.splitDown");
  const ids = paneIds(workspace.activeTab!.root);
  const panes = ids.map((id) => workspace.terminal(id)!);
  const quiet = panes.at(-1)!;
  const floods = panes.slice(0, 3);
  // Every shell prints its prompt before anything is typed.
  await waitFor(() => panes.every((pane) => tail(pane).trim() !== ""), 30_000);
  await sleep(3000);

  const results: ScenarioResult[] = [];
  const doneIn = (pane: TerminalController) => /flood-done/.test(tail(pane));
  results.push(
    await scenario(
      "text 500 MiB in one pane",
      quiet,
      () => type(floods[0]!, "xtask flood text 500"),
      () => doneIn(floods[0]!),
      900_000,
    ),
  );
  const textSeconds = /flood-done ([\d.]+)/.exec(tail(floods[0]!))?.[1];
  await sleep(30_000);
  for (const round of [1, 2, 3]) {
    // The last round's flood-done lines scroll away once `yes` starts; only
    // look for new ones after that.
    const started = performance.now();
    results.push(
      // oxlint-disable-next-line no-await-in-loop -- rounds run one after the other
      await scenario(
        `yes for 60 s in three panes, round ${round}`,
        quiet,
        () => {
          for (const pane of floods) type(pane, "xtask flood yes 60");
        },
        () => performance.now() - started > 2000 && floods.every(doneIn),
        120_000,
      ),
    );
    // oxlint-disable-next-line no-await-in-loop -- the rest between rounds
    await sleep(30_000);
  }

  const report = {
    userAgent: navigator.userAgent,
    firstOutputAt,
    idleUntil,
    textSeconds: textSeconds ? Number(textSeconds) : null,
    grid: { cols: quiet.terminal.cols, rows: quiet.terminal.rows },
    scenarios: results,
    finishedAt: epoch(),
  };
  type(quiet, `xtask flood report ${toHex(JSON.stringify(report))}`);
}
