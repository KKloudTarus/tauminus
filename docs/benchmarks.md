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
