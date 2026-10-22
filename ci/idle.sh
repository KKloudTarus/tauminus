#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Tauminus contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Measures one app idle, the same way for Tauminus and Termius:
#
#   ci/idle.sh <label> <window title> <output dir> <command...>
#
# - window_ms: from exec to a mapped window with that title (Linux, with
#   xdotool on $DISPLAY; null on macOS, where reading windows needs a
#   permission CI runners lack).
# - rss_kib: RSS of the process tree 30 s after launch, shells left out,
#   since Tauminus opens a local shell and Termius opens none. On macOS,
#   WebKit's XPC processes are counted too: they are not the app's children.
#
# Writes <output dir>/idle-<label>.json.

set -euo pipefail

label=$1
title=$2
out=$3
shift 3
mkdir -p "$out"

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

tree() {
  local pid=$1
  echo "$pid"
  for child in $(pgrep -P "$pid" || true); do
    tree "$child"
  done
}

started=$(now_ms)
"$@" > "$out/idle-$label.log" 2>&1 &
pid=$!

window_ms=null
if [ "$(uname)" = Linux ]; then
  for _ in $(seq 1 600); do
    if xdotool search --onlyvisible --name "$title" > /dev/null 2>&1; then
      window_ms=$(($(now_ms) - started))
      break
    fi
    sleep 0.1
  done
fi

remaining=$((30 - ($(now_ms) - started) / 1000))
if [ "$remaining" -gt 0 ]; then sleep "$remaining"; fi
pids=$(tree "$pid")
if [ "$(uname)" = Darwin ] && [ "$label" = tauminus ]; then
  pids="$pids $(pgrep -f 'com.apple.WebKit' || true)"
fi
rss=0
for p in $pids; do
  case $(ps -o comm= -p "$p" 2> /dev/null) in
    *bash | *sh | *zsh | *fish | "") ;;
    *) rss=$((rss + $(ps -o rss= -p "$p" 2> /dev/null || echo 0))) ;;
  esac
done

for p in $pids; do kill "$p" 2> /dev/null || true; done
wait "$pid" 2> /dev/null || true
printf '{"label": "%s", "window_ms": %s, "rss_kib": %s}\n' "$label" "$window_ms" "$rss" | tee "$out/idle-$label.json"
