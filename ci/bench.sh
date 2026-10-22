#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Tauminus contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Runs the in-app benchmark (apps/desktop/src/bench/run.ts) on Linux or macOS
# and samples memory once a second while it runs.
#
#   ci/bench.sh <app binary> <dir with the xtask binary> <output dir>
#
# The app must be a benchmark build (VITE_TAUMINUS_BENCH=1). On Linux it runs
# on the X display in $DISPLAY. On macOS WebKit runs its web content in XPC
# processes that are not the app's children, so they are found by name; the
# runner has no other WebKit app open.

set -euo pipefail

app=$1
# Absolute: the app's shells start in the home directory, not here.
tools=$(cd "$2" && pwd)
mkdir -p "$3"
out=$(cd "$3" && pwd)
rm -f "$out/report.json" "$out/samples.csv"

now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }

# Prints the PIDs of $1 and all its descendants.
tree() {
  local pid=$1
  echo "$pid"
  for child in $(pgrep -P "$pid" || true); do
    tree "$child"
  done
}

# Prints those of the given PIDs whose command is a WebKit process.
webkit_only() {
  for pid in "$@"; do
    if ps -o comm= -p "$pid" 2> /dev/null | grep -q '^WebKit'; then
      echo "$pid"
    fi
  done
}

rss_kib() {
  local total=0
  for pid in "$@"; do
    total=$((total + $(ps -o rss= -p "$pid" 2> /dev/null || echo 0)))
  done
  echo "$total"
}

export PATH="$tools:$PATH"
if [ "$(uname)" = Darwin ]; then
  # The throwaway runner only: no App Nap for any app, as for a frontmost one.
  defaults write NSGlobalDomain NSAppSleepDisabled -bool YES
fi
export TAUMINUS_BENCH_OUT="$out/report.json"
export TAUMINUS_LOG="${TAUMINUS_LOG:-debug,russh=info}"
now_ms > "$out/launched_ms"
"$app" > "$out/app.log" 2>&1 &
app_pid=$!
if [ "$(uname)" = Darwin ]; then
  # A user's terminal is the frontmost app. Started from a CI shell it is
  # not, and App Nap then coalesces its timers and throttles its drawing.
  (sleep 8 && osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $app_pid) to true" > /dev/null 2>&1 || true) &
fi
if [ "$(uname)" = Linux ]; then
  # No window manager on Xvfb: give the window focus as a user's click would.
  (sleep 8 && xdotool search --sync --onlyvisible --name Tauminus windowactivate > /dev/null 2>&1 || true) &
fi

timeout=$((SECONDS + 2700))
# The first note comes within a minute of launch; none by 180 s means the
# shells never ran, and waiting out the full deadline would show nothing more.
early=$((SECONDS + 180))
while [ ! -s "$out/report.json" ] && [ $SECONDS -lt $timeout ] && kill -0 "$app_pid" 2> /dev/null; do
  if [ $SECONDS -gt $early ] && [ ! -s "$out/report.json.log" ]; then
    echo "no note from the benchmark after 180 s" >&2
    break
  fi
  pids=$(tree "$app_pid")
  if [ "$(uname)" = Darwin ]; then
    web=$(pgrep -f 'com.apple.WebKit' || true)
  else
    # The app's children include its shells; only WebKit's processes count.
    web=$(webkit_only $pids)
  fi
  # shellcheck disable=SC2086 # word splitting turns the lists into arguments
  echo "$(now_ms),$(rss_kib "$app_pid"),$(rss_kib $web)" >> "$out/samples.csv"
  sleep 1
done

if [ ! -s "$out/report.json" ]; then
  # What the window showed when the run gave up.
  if [ "$(uname)" = Darwin ]; then
    screencapture -x "$out/screen.png" || true
  else
    import -window root "$out/screen.png" || true
  fi
fi
kill "$app_pid" 2> /dev/null || true
wait "$app_pid" 2> /dev/null || true
echo "== notes from the run"
cat "$out/report.json.log" 2> /dev/null || echo "(none)"
if [ ! -s "$out/report.json" ]; then
  echo "the benchmark wrote no report; app log:" >&2
  tail -50 "$out/app.log" >&2
  exit 1
fi
python3 "$(dirname "$0")/bench_summary.py" "$out"
