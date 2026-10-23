// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import { Broadcast, typedByUser } from "./broadcast.ts";

describe("Broadcast", () => {
  it("types from a member into the other members only", () => {
    const broadcast = new Broadcast();
    let changes = 0;
    broadcast.subscribe(() => (changes += 1));
    broadcast.toggle("a");
    broadcast.toggle("b");
    broadcast.toggle("c");
    expect(broadcast.targets("a")).toEqual(["b", "c"]);
    expect(broadcast.targets("z")).toEqual([]);
    broadcast.toggle("b");
    expect(broadcast.targets("a")).toEqual(["c"]);
    broadcast.remove("c");
    broadcast.remove("c");
    expect([...broadcast.getSnapshot()]).toEqual(["a"]);
    broadcast.clear();
    broadcast.clear();
    expect(changes).toBe(6);
  });
});

describe("typedByUser", () => {
  it.each(["ls", "\r", "\u0003", "\u001b[A", "\u001bb", "\u001b[200~x\u001b[201~"])(
    "%j is the user's",
    (data) => expect(typedByUser(data)).toBe(true),
  );
  it.each([
    "\u001b[12;40R",
    "\u001b[?2004;1$y",
    "\u001b[20;2$y",
    "\u001b[8;24;80t",
    "\u001b[?1;2c",
    "\u001b[>0;276;0c",
    "\u001b[I",
    "\u001b[O",
    "\u001b[<0;10;5M",
    "\u001b[M !!",
    "\u001b]11;rgb:0000/0000/0000\u0007",
    "\u001bP1$r0m\u001b\\",
  ])("%j is the terminal's", (data) => expect(typedByUser(data)).toBe(false));
});
