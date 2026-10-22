# SPDX-FileCopyrightText: 2026 Tauminus contributors
# SPDX-License-Identifier: GPL-3.0-or-later

"""Summarizes one benchmark run for docs/benchmarks.md and the job summary.

Inputs, in the run's output directory:
  report.json   written by the app's benchmark page (src/bench/run.ts)
  samples.csv   epoch_ms,app_kib,web_kib, one row a second, from the CI script
  launched_ms   epoch ms just before the app was started
  termius.json  optional: {"window_ms": ..., "rss_kib": ...} for Termius

Writes summary.json and summary.md next to them and prints the Markdown.
"""

import csv
import json
import sys
from pathlib import Path


def mib(kib):
    return round(kib / 1024)


def ms(value):
    return "n/a" if value is None else round(value)


def main(directory):
    out = Path(directory)
    report = json.loads((out / "report.json").read_text())
    launched = int((out / "launched_ms").read_text().strip())
    samples = []
    with open(out / "samples.csv", newline="") as file:
        for row in csv.reader(file):
            if len(row) == 3 and row[0].isdigit():
                samples.append((int(row[0]), int(row[1]), int(row[2])))

    def total(sample):
        return sample[1] + sample[2]

    def last_before(at):
        earlier = [s for s in samples if s[0] <= at]
        return earlier[-1] if earlier else None

    def between(start, end):
        return [s for s in samples if start <= s[0] <= end]

    summary = {
        "userAgent": report["userAgent"],
        "coldStartMs": round(report["firstOutputAt"] - launched),
        "grid": report["grid"],
        "scenarios": [],
    }
    idle = last_before(report["idleUntil"])
    if idle:
        summary["idle"] = {"appMiB": mib(idle[1]), "webMiB": mib(idle[2]), "totalMiB": mib(total(idle))}
    for scenario in report["scenarios"]:
        before = last_before(scenario["startedAt"])
        during = between(scenario["startedAt"], scenario["endedAt"])
        after = last_before(scenario["endedAt"] + 25_000)
        settle = between(scenario["endedAt"] + 5_000, scenario["endedAt"] + 25_000)
        entry = {
            "name": scenario["name"],
            "completed": scenario["completed"],
            "seconds": round((scenario["endedAt"] - scenario["startedAt"]) / 1000, 1),
            "droppedPercent": round(scenario["droppedShare"] * 100, 1),
            "maxGapMs": round(scenario["maxGapMs"]),
            "latencyMs": scenario["latencyMs"],
        }
        if before and after and during:
            entry["webMiB"] = {
                "before": mib(before[2]),
                "peak": mib(max(s[2] for s in during)),
                "after": mib(after[2]),
            }
            entry["appMiB"] = {"before": mib(before[1]), "after": mib(after[1])}
        if settle:
            # How far the webview's memory moved once the run was over.
            entry["webSettleBandMiB"] = mib(max(s[2] for s in settle) - min(s[2] for s in settle))
        summary["scenarios"].append(entry)
    if report.get("textSeconds") is not None:
        summary["textMiBPerSecond"] = round(500 / report["textSeconds"], 1)
    termius_file = out / "termius.json"
    if termius_file.exists():
        summary["termius"] = json.loads(termius_file.read_text())

    (out / "summary.json").write_text(json.dumps(summary, indent=2))
    lines = [
        f"### {summary['userAgent']}",
        "",
        f"- Cold start, exec to first shell output: {summary['coldStartMs']} ms",
        f"- Pane grid: {summary['grid']['cols']} x {summary['grid']['rows']}",
    ]
    if "idle" in summary:
        idle = summary["idle"]
        lines.append(
            f"- Idle, one local tab: app {idle['appMiB']} MiB, webview {idle['webMiB']} MiB, total {idle['totalMiB']} MiB RSS"
        )
    if "termius" in summary:
        lines.append(f"- Termius on the same runner: {json.dumps(summary['termius'])}")
    lines += [
        "",
        "| Scenario | Done | Time | Dropped frames | Latency p50 / p95 / p99 / max | Webview RSS before / peak / after | Band after |",
        "|---|---|---|---|---|---|---|",
    ]
    for entry in summary["scenarios"]:
        latency = entry["latencyMs"]
        web = entry.get("webMiB")
        memory = f"{web['before']} / {web['peak']} / {web['after']} MiB" if web else "n/a"
        lines.append(
            f"| {entry['name']} | {'yes' if entry['completed'] else 'no'} | {entry['seconds']} s "
            f"| {entry['droppedPercent']}% | {ms(latency['p50'])} / {ms(latency['p95'])} / {ms(latency.get('p99'))} / {ms(latency['max'])} ms (n={latency['n']}) "
            f"| {memory} | {entry.get('webSettleBandMiB', 'n/a')} MiB |"
        )
    text = "\n".join(lines) + "\n"
    (out / "summary.md").write_text(text)
    print(text)


if __name__ == "__main__":
    main(sys.argv[1])
