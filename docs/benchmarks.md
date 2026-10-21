# Benchmarks

Measurements that back performance claims. Each entry records the build, the
machine, and the exact command, so it can be repeated.

## 2026-10-21: local shell output through the full pipeline

- Build: debug (`tauri build --debug --no-bundle`), commit of the skeleton.
- Machine: Linux 6.8, Ubuntu 24.04, WebKitGTK 2.52.6, Xvfb without GPU, so
  xterm.js rendered without hardware acceleration.
- Window: 148 x 50 cells, scrollback 10,000 lines.
- Command, typed into the running app:

  ```sh
  time cat big.txt   # 100,833,334 bytes of base64, 120 columns per line
  ```

| Metric | Result |
|---|---|
| Wall time | 11.4 s (8.8 MB/s end to end) |
| Rust process RSS | 145 MB before, 133 MB after |
| WebKit web process RSS | 297 MB after the run |
| Responsiveness | `echo` typed right after the run printed immediately |

Rust memory did not grow while 100 MB streamed through, which shows the
backpressure path holding. Release builds on real GPUs and on macOS and
Windows webviews are still to be measured, as the v0.1 exit criteria require.

## 2026-10-21: DOM renderer on WebKitGTK

Same command and file size as above, on a second Linux 5.15 machine with
WebKitGTK and Xvfb, after WebGL was found to skip redraws on WebKitGTK.

| Metric | Result |
|---|---|
| Wall time | 13.5 s (7.5 MB/s end to end) |
| Rust process RSS | 158 MB after the run |
| WebKit web process RSS | 222 MB after the run |

The machines differ, so the 15% gap against the WebGL run is indicative.

## 2026-10-21: russh against OpenSSH (spike)

See [feature-parity.md](feature-parity.md#verified-by-experiment): 263 to
278 MiB/s for 256 MiB of stdout over one SSH channel on loopback.

## 2026-10-21: four panes, cold start, idle memory (v0.1 exit criteria)

- Build: release (`cargo build --release -p tauminus-desktop --features
  tauri/custom-protocol`), DOM renderer.
- Machine: Intel Xeon Platinum 8168, 96 vCPUs shared with other work (load
  average 13 to 15 during the runs), Linux 5.15, Ubuntu 24.04, WebKitGTK
  2.52.6, Xvfb at 1280 x 800 with no GPU. One run per row, so treat
  differences under about 10% as noise.
- Layout: one tab split into four panes of about 57 x 23 cells each,
  scrollback 10,000 lines.
- `big500.txt`: 504,166,667 bytes of base64, 120 columns per line.

Frame and latency numbers come from a probe added to the page for these runs
only and not committed. It counts `requestAnimationFrame` callbacks and
treats each gap of g ms as round(g / 16.7) − 1 dropped frames. For latency
it stamps a keydown in the quiet pane and stops the clock at that pane's
first xterm.js render after output was parsed, so the figure covers the
round trip through the shell. The quiet pane (bottom right) got one keystroke
every 250 ms for the whole run. RSS was sampled every second from
`/proc/<pid>/smaps_rollup`; "first" is about 2 s into the run, "last" about
15 s after it ended.

Before: each pane wrote straight into its own xterm.js instance.

| Run | Wall time | Dropped frames | Latency p50 / p95 / max | WebKit RSS first / peak / last | Rust RSS |
|---|---|---|---|---|---|
| `cat big500.txt`, local, 3 panes idle | 70.5 s (7.2 MB/s) | 4.4% | 15 / 29 / 50 ms | 261 / 326 / 315 MB | 150 MB flat |
| `timeout 60 yes` in 3 panes | 60 s | 41.4% | 41 / 70 / 91 ms | 534 / 747 / 674 MB | 149 to 154 MB |

After: `OutputScheduler` gives one pane a turn at a time, about 6 ms of
parsing each (see [architecture.md](architecture.md#the-view-side)).

| Run | Wall time | Dropped frames | Latency p50 / p95 / max | WebKit RSS first / peak / last | Rust RSS |
|---|---|---|---|---|---|
| `cat big500.txt`, local | 89.6 s (5.6 MB/s) | 4.1% | 18 / 28 / 41 ms | 252 / 306 / 295 MB | 150 to 153 MB |
| `cat big500.txt` over SSH to 127.0.0.1 | 76.5 s (6.6 MB/s) | 2.0% | 16 / 24 / 52 ms | 258 / 352 / 336 MB | 154 to 158 MB |
| `timeout 60 yes` in 3 panes, first run | 60 s | 2.7% | 27 / 34 / 61 ms | 511 / 524 / 516 MB | 149 to 153 MB |
| `timeout 60 yes` in 3 panes, second run | 60 s | 2.2% | 26 / 32 / 60 ms | 514 / 525 / 516 MB | 149 to 153 MB |

Variants tried on the way:

| Variant | Result | Why not |
|---|---|---|
| Fixed 8 KiB per turn | `cat` 138.1 s | one timer hop of about 1 ms per 8 KiB |
| Burst sized from wall time per turn | `cat` 101.7 s | a paint inside the turn made the rate look slow |
| Burst sized from measured parse rate, 6 ms | `cat` 82.7 s, `yes` 3.7% dropped, worst keystroke 39 ms | one stuck pane stopped every pane, and a cheap turn could inflate the next burst |
| Same, 8 ms | `yes` 8.1% dropped, `cat` 81.7 s | more dropped frames, no faster |

The three-pane flood goes from 41% dropped frames to under 3%, and the
worst keystroke from 91 ms to about 60 ms. Single-pane `cat` pays for it:
89.6 s against 70.5 s before, or 76.5 s over SSH. Against the targets on
this machine: dropped frames stay under 5% in every run; input latency is
under 50 ms at p95 in every run, while the worst single keystroke reached
52 to 61 ms in three of the four runs; WebKit RSS at the end is within
50 MB of the start for `yes` and local `cat` and 78 MB above it for `cat`
over SSH.

WebKit memory does not return to its idle level after a flood. Four `cat`
runs back to back in one app, before the scheduler, ended at 315, 378, 380
and 355 MB: it climbs for two runs and then holds, which points to the
JavaScript heap keeping its high-water mark, not to a leak.

### Cold start and idle memory

Cold start is the time from `exec` of the binary to the first render of the
shell prompt, taken as the epoch time stamped right before `exec` and
`performance.timeOrigin + performance.now()` in the page. Seven launches
with a warm page cache: 1089, 1103, 1104, 1106, 1115, 1149 and 1295 ms,
median 1106 ms.

Idle, one local tab, 30 s after launch:

| Process | RSS | PSS |
|---|---|---|
| tauminus-desktop | 143 MB | 74 MB |
| WebKitWebProcess | 179 MB | 113 MB |
| WebKitNetworkProcess | 48 MB | 21 MB |
| Total | 370 MB | 208 MB |

PSS splits shared libraries between the processes that map them, so it is
the fairer total. Termius is not installed on this machine, so the
comparison the roadmap asks for is still to do, as are the macOS and
Windows webviews.
