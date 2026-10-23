// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Imported first by the player's worker, for this effect alone. The
// headless xterm.js 6.0.0 checks `"requestIdleCallback" in window` as it
// loads, whenever it is not under Node, which throws in a worker, where
// there is no `window`. A worker has no `requestIdleCallback` either, so
// with `window` naming the worker's own scope that check is false and
// xterm.js takes the path made for it.
if (!("window" in globalThis)) Reflect.set(globalThis, "window", globalThis);
