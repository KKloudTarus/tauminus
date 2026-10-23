// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it, vi } from "vitest";

import type { GuardFinding, GuardVerdict } from "@/ipc/index.ts";

import {
  CHECK_TIMEOUT_MS,
  CommandGuard,
  ECHO_WAIT_MS,
  SECRET_PROMPT,
  type GuardRequest,
  type GuardSource,
  type Marker,
  type Screen,
  afterPrompts,
  readLine,
} from "./command-guard.ts";

/** A screen of fixed-width rows; `wrapped` marks rows that continue the one above. */
class FakeScreen implements Screen {
  type: "normal" | "alternate" = "normal";
  rows: { text: string; wrapped: boolean }[] = [{ text: "", wrapped: false }];
  cursorX = 0;
  cursorY = 0;
  baseY = 0;
  readonly cols: number;

  get length(): number {
    return this.rows.length;
  }

  constructor(cols = 20) {
    this.cols = cols;
  }

  getLine(y: number) {
    const row = this.rows[y];
    if (!row) return undefined;
    return {
      isWrapped: row.wrapped,
      translateToString: (trimRight = false, start = 0, end = this.cols) => {
        const text = row.text.padEnd(this.cols).slice(start, end);
        return trimRight ? text.trimEnd() : text;
      },
    };
  }

  /** Writes like a terminal echo: wraps at the edge and moves the cursor. */
  echo(text: string): void {
    for (const char of text) {
      const y = this.baseY + this.cursorY;
      const row = this.rows[y] ?? { text: "", wrapped: false };
      row.text = row.text.padEnd(this.cursorX).slice(0, this.cursorX) + char;
      this.rows[y] = row;
      this.cursorX += 1;
      if (this.cursorX === this.cols) {
        this.rows.push({ text: "", wrapped: true });
        this.cursorX = 0;
        this.cursorY += 1;
      }
    }
  }

  newline(): void {
    this.rows.push({ text: "", wrapped: false });
    this.cursorX = 0;
    this.cursorY += 1;
  }
}

class FakeMarker implements Marker {
  line: number;
  disposed = false;

  constructor(line: number) {
    this.line = line;
  }

  dispose(): void {
    this.disposed = true;
  }
}

/** One keystroke per character; the test strings are ASCII. */
const keys = (text: string) => text.split("");

const DANGER: GuardFinding = { rule: "recursive-delete", reason: "deletes", command: "rm -rf /" };

/**
 * Flags any line containing `rm -rf` once line continuations are joined; a
 * line ending in `\` is incomplete.
 */
class FakeSource implements GuardSource {
  on = true;
  checked: string[] = [];
  fail = false;

  active(): boolean {
    return this.on;
  }

  subscribe(): () => void {
    return () => {};
  }

  async check(lines: string[]): Promise<GuardVerdict> {
    this.checked.push(...lines);
    if (this.fail) throw new Error("ipc down");
    const danger = lines.some((line) => line.replaceAll("\\\n", "").includes("rm -rf"));
    return {
      findings: danger ? [DANGER] : [],
      incomplete: lines[0]?.endsWith("\\") ?? false,
    };
  }
}

function setup(answer = false) {
  const screen = new FakeScreen();
  const source = new FakeSource();
  const sent: string[] = [];
  const asked: GuardRequest[] = [];
  const markers: FakeMarker[] = [];
  const link = { lagging: false, parses: 0, waiters: [] as ((parsed: boolean) => void)[] };
  /** Output reaches the screen and is parsed, as an echo does. */
  const arrive = (text: string) => {
    screen.echo(text);
    link.parses += 1;
    for (const waiter of link.waiters.splice(0)) waiter(true);
  };
  const guard = new CommandGuard(source, {
    send: (data) => {
      sent.push(data);
      // The shell echoes printable input, at once unless the link lags.
      if (data >= " " && !data.startsWith("\u001b") && !link.lagging) arrive(data);
    },
    parsed: () => link.parses,
    nextParse: (ms) =>
      new Promise((resolve) => {
        link.waiters.push(resolve);
        setTimeout(() => resolve(false), ms);
      }),
    screen: () => screen,
    mark: () => {
      const marker = new FakeMarker(screen.baseY + screen.cursorY);
      markers.push(marker);
      return marker;
    },
    ask: async (request) => {
      asked.push(request);
      return answer;
    },
  });
  const type = async (...input: string[]) => {
    for (const key of input) guard.input(key);
    await guard.settled();
  };
  return { screen, source, sent, asked, markers, guard, type, link, arrive };
}

describe("SECRET_PROMPT", () => {
  it.each([
    "[sudo] password for me: ",
    "[sudo] password for me: ****",
    "Password for 'https://u@github.com': ",
    "Enter MFA code for arn:aws:iam::1:mfa/me: ",
    "Unseal Key (will be hidden): ",
    "Enter passphrase for key '/home/me/.ssh/id': ",
    "Password: (press TAB for no echo) ",
  ])("knows %j asks for a secret", (line) => expect(SECRET_PROMPT.test(line)).toBe(true));

  it.each(["me@host:~/passwords$ ", "$ echo token", "Do you want to continue? [Y/n] y"])(
    "knows %j does not",
    (line) => expect(SECRET_PROMPT.test(line)).toBe(false),
  );
});

describe("readLine", () => {
  it("reads from the anchor to the end of the wrapped line", () => {
    const screen = new FakeScreen(10);
    screen.echo("$ ");
    const anchor = { line: 0, x: screen.cursorX };
    screen.echo("echo 12345678");
    expect(readLine(screen, anchor)).toEqual({
      anchored: "echo 12345678",
      whole: "$ echo 12345678",
      below: "",
    });
  });

  it("falls back to the whole line when the anchor is elsewhere or after the cursor", () => {
    const screen = new FakeScreen(10);
    screen.echo("out");
    screen.newline();
    screen.echo("$ ls");
    expect(readLine(screen, { line: 0, x: 0 }).anchored).toBe("$ ls");
    expect(readLine(screen, { line: 1, x: 8 }).anchored).toBe("$ ls");
    expect(readLine(screen, undefined).anchored).toBe("$ ls");
  });
});

describe("readLine below the cursor", () => {
  it("reads the rows with text right after the cursor's line", () => {
    const screen = new FakeScreen(10);
    screen.echo("$ ");
    const anchor = { line: 0, x: 2 };
    screen.echo("echo a");
    // zsh shows the rest of a recalled multi-line command below the cursor.
    screen.rows.push({ text: "rm -rf /", wrapped: false }, { text: "", wrapped: false });
    screen.rows.push({ text: "unrelated", wrapped: false });
    expect(readLine(screen, anchor).below).toBe("echo a\nrm -rf /");
  });
});

describe("afterPrompts", () => {
  it("reads what follows each prompt ending", () => {
    expect(afterPrompts("/ # rm -rf /srv")).toEqual(["rm -rf /srv"]);
    expect(afterPrompts("prod> rm -rf /srv")).toEqual(["rm -rf /srv"]);
    expect(afterPrompts("ls")).toEqual([]);
    expect(afterPrompts("$ $ $ $ $ x")).toHaveLength(4);
    expect(afterPrompts("a > b # $ % > rm -rf /srv")[0]).toBe("rm -rf /srv");
  });
});

describe("CommandGuard", () => {
  it("sends a line the backend passes, after checking what the user typed", async () => {
    const { screen, source, sent, asked, type } = setup();
    screen.echo("user@db:~$ ");
    await type(...keys("ls -la"), "\r");
    expect(source.checked).toEqual(["ls -la", "user@db:~$ ls -la"]);
    expect(asked).toEqual([]);
    expect(sent.join("")).toBe("ls -la\r");
  });

  it("holds back Enter on a dangerous line until the user confirms", async () => {
    const declined = setup(false);
    await declined.type(...keys("rm -rf /"), "\r");
    expect(declined.asked).toEqual([{ kind: "command", text: "rm -rf /", findings: [DANGER] }]);
    expect(declined.sent.join("")).toBe("rm -rf /");

    const confirmed = setup(true);
    await confirmed.type(...keys("rm -rf /"), "\r");
    expect(confirmed.sent.join("")).toBe("rm -rf /\r");
  });

  it.each(["\n", "\u001b\r", "\u000f"])("checks %j as Enter", async (key) => {
    const { sent, asked, type } = setup();
    await type(...keys("rm -rf /"), key);
    expect(asked).toHaveLength(1);
    expect(sent).not.toContain(key);
  });

  it("keeps later input in order behind a pending check", async () => {
    const { sent, guard } = setup(true);
    for (const key of [...keys("rm -rf /"), "\r", ...keys("ls")]) guard.input(key);
    await guard.settled();
    expect(sent.join("")).toBe("rm -rf /\rls");
  });

  it("checks a continued line together with the lines before it", async () => {
    const { screen, source, asked, type } = setup();
    await type(...keys("r\\"), "\r");
    screen.newline();
    screen.echo("> ");
    await type(...keys("m -rf /"), "\r");
    expect(source.checked).toContain("r\\\nm -rf /");
    expect(asked).toHaveLength(1);
  });

  it("forgets the continued line on Ctrl+C", async () => {
    const { screen, source, type } = setup();
    await type(...keys("echo \\"), "\r", "\u0003");
    screen.newline();
    await type(...keys("ls"), "\r");
    expect(source.checked.at(-1)).toBe("ls");
  });

  it("asks before any multi-line paste, with what it would run", async () => {
    const { sent, asked, type } = setup(false);
    await type("\u001b[200~echo a\rrm -rf /\r\u001b[201~");
    expect(asked).toEqual([{ kind: "paste", text: "echo a\nrm -rf /\n", findings: [DANGER] }]);
    expect(sent).toEqual([]);

    const ok = setup(true);
    await ok.type("echo a\recho b\r");
    expect(ok.asked[0]?.findings).toEqual([]);
    expect(ok.sent).toEqual(["echo a\recho b\r"]);
  });

  it("asks when the backend cannot check", async () => {
    const { source, asked, type } = setup();
    source.fail = true;
    await type(...keys("ls"), "\r");
    expect(asked[0]?.findings[0]?.rule).toBe("unchecked");
  });

  it("passes input straight through when the host is not guarded", async () => {
    const { source, sent, asked, type } = setup();
    source.on = false;
    await type(...keys("rm -rf /"), "\r", "a\rb\r");
    expect(source.checked).toEqual([]);
    expect(asked).toEqual([]);
    expect(sent.join("")).toBe("rm -rf /\ra\rb\r");
  });

  it("sends an empty Enter without a check", async () => {
    const { source, sent, type } = setup();
    await type("\r");
    expect(source.checked).toEqual([]);
    expect(sent).toEqual(["\r"]);
  });

  it("disposes the marker of each submitted line", async () => {
    const { markers, type } = setup();
    await type(...keys("ls"), "\r");
    expect(markers).toHaveLength(1);
    expect(markers[0]?.disposed).toBe(true);
  });

  it("reads the whole line when the anchor was taken in the other buffer", async () => {
    const { screen, source, type } = setup();
    screen.echo("$ ");
    await type("l");
    screen.type = "alternate";
    await type("s", "\r");
    expect(source.checked).toEqual(["$ ls", "ls"]);
  });

  it("does not read prompt endings out of quoted text when typing has a start", async () => {
    const { screen, source, asked, type } = setup();
    screen.echo("$ ");
    await type(...keys('git commit -m "guard > rm -rf handling"'), "\r");
    // The fake flags any "rm -rf"; what matters is that no reading starts there.
    expect(source.checked.filter((line) => line.startsWith("rm -rf"))).toEqual([]);
    expect(asked).toHaveLength(1);
  });

  it("checks and shows a paste that ends a line with what is already on it", async () => {
    const { screen, asked, sent, type } = setup(false);
    screen.echo("user@db:~$ ");
    await type(...keys("rm -rf /srv "));
    await type("echo done\r");
    expect(asked).toEqual([{ kind: "paste", text: "rm -rf /srv echo done\n", findings: [DANGER] }]);
    expect(sent.join("")).toBe("rm -rf /srv ");

    // A paste that leaves the line open waits for Enter, which reads it all.
    const open = setup(false);
    await open.type(...keys("rm -rf /srv "), "\u001b[200~a\u001b[201~");
    expect(open.asked).toEqual([]);
  });

  it("sends at most 8 pieces of a paste to the check", async () => {
    const { source, asked, type } = setup(false);
    await type(Array.from({ length: 12 }, (_, n) => `p${n}`).join("\u0003") + "\r");
    expect(source.checked).toHaveLength(8);
    expect(asked[0]?.findings.map((f) => f.rule)).toEqual(["control-characters"]);
  });

  it("reads past a root prompt when the start of typing is lost", async () => {
    const { screen, source, asked, type } = setup();
    screen.echo("/ # ");
    screen.type = "alternate";
    await type(...keys("rm -rf /srv"));
    screen.type = "normal";
    await type("\u001b[D", "\r");
    expect(source.checked).toContain("rm -rf /srv");
    expect(asked).toHaveLength(1);
  });

  it("checks the keys typed when the echo has not arrived", async () => {
    const { source, asked, link, type } = setup();
    link.lagging = true;
    await type(...keys("rm -rf /"), "\r");
    expect(source.checked).toContain("rm -rf /");
    expect(asked).toHaveLength(1);
  });

  it("follows erasing keys in the typed copy", async () => {
    const { source, link, type } = setup();
    link.lagging = true;
    await type(...keys("rm -rf /x"), "\u007f", "\u007f", "\u0017", ...keys("ls"), "\r");
    // Ctrl+W removes the word before the cursor, as readline does.
    expect(source.checked).toEqual(["rm ls"]);
  });

  it("waits for the echo of a recalled line before reading the screen", async () => {
    const { source, asked, link, guard, arrive } = setup();
    link.lagging = true;
    // Up arrow: the shell redraws the line from history after a round trip.
    guard.input("\u001b[A");
    guard.input("\r");
    setTimeout(() => arrive("rm -rf /"), 50);
    await guard.settled();
    expect(source.checked).toEqual(["rm -rf /"]);
    expect(asked).toHaveLength(1);
  });

  it("reads the screen as it is when no echo comes", async () => {
    const { source, sent, link, guard } = setup();
    link.lagging = true;
    guard.input("\u001b[A");
    guard.input("\r");
    await guard.settled();
    expect(source.checked).toEqual([]);
    expect(sent).toEqual(["\u001b[A", "\r"]);
  });

  it("asks before Ctrl+X Ctrl+E opens the line in an editor", async () => {
    const { sent, asked, type } = setup(false);
    await type(...keys("ls"), "\u0018", "\u0005");
    expect(asked[0]?.findings[0]?.rule).toBe("edit-and-run");
    expect(sent).not.toContain("\u0005");
  });

  it("treats keypad Enter as Enter", async () => {
    const { asked, sent, type } = setup(false);
    await type(...keys("rm -rf /"), "\u001bOM");
    expect(asked).toHaveLength(1);
    expect(sent).not.toContain("\u001bOM");
  });

  it("flags a paste whose control characters act as keys, and checks each piece", async () => {
    const { source, asked, type } = setup(false);
    // Ctrl+C drops the harmless start; the shell then runs the rest.
    await type('echo "safe \u0003rm -rf ~\r');
    expect(source.checked).toEqual(['echo "safe ', "rm -rf ~\n"]);
    expect(asked[0]?.findings.map((f) => f.rule)).toEqual([
      "control-characters",
      "recursive-delete",
    ]);
  });

  it("asks before a paste that holds Ctrl+O without a newline", async () => {
    const { sent, asked, type } = setup(false);
    await type("rm -rf ~\u000f");
    expect(asked[0]?.kind).toBe("paste");
    expect(sent).toEqual([]);
  });

  it("passes keys and terminal replies through without asking", async () => {
    const { sent, asked, type } = setup();
    const replies = [
      "\u001b[A",
      "\u001b[12;5R",
      "\u001b]11;rgb:ffff/ffff/ffff\u001b\\",
      "\u001bP1$r0m\u001b\\",
      "\u001b]10;rgb:0/0/0\u0007",
    ];
    await type(...replies);
    expect(asked).toEqual([]);
    expect(sent).toEqual(replies);
  });

  it("sends a one-line bracketed paste without asking", async () => {
    const { sent, asked, type } = setup();
    await type("\u001b[200~ls -la\u001b[201~");
    expect(asked).toEqual([]);
    expect(sent).toEqual(["\u001b[200~ls -la\u001b[201~"]);
  });

  it("passes Alt with any key as a key", async () => {
    const { sent, asked, type } = setup();
    await type("\u001b\u007f", "\u001b\b", "\u001b\u0001", "\u001bx");
    expect(asked).toEqual([]);
    expect(sent).toHaveLength(4);
  });

  it("does not check or show a password typed at a prompt that hides it", async () => {
    const { screen, source, sent, link, type } = setup();
    screen.echo("[sudo] password for ops: ");
    link.lagging = true;
    await type(...keys("hunt3r$x"), "\r");
    expect(source.checked).toEqual([]);
    expect(sent.at(-1)).toBe("\r");
  });

  it.each([
    ["Up", ["\u001b[A"]],
    ["a paste", ["\u001b[200~rm -rf /srv\u001b[201~"]],
  ])("checks %s at a password prompt that has not redrawn yet", async (_, input) => {
    const { screen, source, link, guard, arrive } = setup();
    screen.echo("[sudo] password for ops: ");
    link.lagging = true;
    for (const data of input) guard.input(data);
    guard.input("\r");
    setTimeout(() => arrive("rm -rf /srv"), 50);
    await guard.settled();
    expect(source.checked.length).toBeGreaterThan(0);
  });

  it("does not take a prompt that only ends in a secret word's letters", async () => {
    const { screen, source, link, type } = setup();
    screen.echo("kingpin: ");
    link.lagging = true;
    await type(...keys("rm -rf /"), "\r");
    expect(source.checked).toContain("rm -rf /");
  });

  it("checks a line typed after a password-like prompt when it shows", async () => {
    const { screen, asked, type } = setup();
    screen.echo("Password: ");
    await type(...keys("rm -rf /"), "\r");
    expect(asked).toHaveLength(1);
  });

  it.each([[["\u001b#"]], [["\u001b", "v"]]])(
    "asks before %j runs a changed line",
    async (keysToSend) => {
      const { asked, type } = setup(false);
      await type(...keys("ls"), ...keysToSend);
      expect(asked[0]?.findings[0]?.rule).toMatch(/comment-and-run|edit-and-run/);
    },
  );

  it("reads from where typing started after a confirmed paste", async () => {
    const { screen, source, type } = setup(true);
    screen.echo("/ # ");
    await type(...keys("rm -rf "));
    await type("/srv\recho\r");
    await type("\r");
    // The whole line is `/ # rm -rf ...`, which reads as a comment; the
    // anchor keeps the command itself.
    expect(source.checked.some((line) => line.startsWith("rm -rf "))).toBe(true);
  });

  it("asks when a check does not answer in time", async () => {
    vi.useFakeTimers();
    try {
      const { source, asked, guard } = setup(false);
      source.check = () => new Promise(() => {});
      for (const key of keys("ls")) guard.input(key);
      guard.input("\r");
      await vi.advanceTimersByTimeAsync(CHECK_TIMEOUT_MS + ECHO_WAIT_MS + 10);
      expect(asked[0]?.findings[0]?.rule).toBe("unchecked");
    } finally {
      vi.useRealTimers();
    }
  });

  it("sends Ctrl+C at once and drops what waited behind a check", async () => {
    const { sent, guard, source } = setup(true);
    let release: (() => void) | undefined;
    const check = source.check.bind(source);
    source.check = async (lines) => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return check(lines);
    };
    for (const key of [...keys("ls"), "\r", ...keys("pwd")]) guard.input(key);
    await expect.poll(() => release).toBeDefined();
    guard.interrupt();
    expect(sent.join("")).toBe("ls\u0003");
    release?.();
    await guard.settled();
    // Nothing under check or behind it is asked about or sent: Ctrl+C
    // cleared that line.
    expect(sent.join("")).toBe("ls\u0003");
  });

  it("sends nothing a question approved after Ctrl+C was about", async () => {
    let approve: ((yes: boolean) => void) | undefined;
    const screen = new FakeScreen();
    const source = new FakeSource();
    const sent: string[] = [];
    const guard = new CommandGuard(source, {
      send: (data) => sent.push(data),
      parsed: () => 0,
      nextParse: async () => false,
      screen: () => screen,
      mark: () => new FakeMarker(0),
      ask: () =>
        new Promise<boolean>((resolve) => {
          approve = resolve;
        }),
    });
    guard.input("echo a\rrm -rf /\r");
    await expect.poll(() => approve).toBeDefined();
    guard.interrupt();
    approve?.(true);
    await guard.settled();
    expect(sent).toEqual(["\u0003"]);
  });

  it.each([
    ["while Enter waits for the echo", ["r", "m", "\r"]],
    ["while a paste is checked", ["echo a\recho b\r"]],
  ])("drops what Ctrl+C cut off %s", async (_, input) => {
    const { sent, guard, source, link } = setup(true);
    link.lagging = true;
    let release: (() => void) | undefined;
    const check = source.check.bind(source);
    source.check = async (lines) => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return check(lines);
    };
    for (const key of input) guard.input(key);
    await new Promise((resolve) => setTimeout(resolve, 10));
    guard.interrupt();
    release?.();
    await guard.settled();
    expect(sent.at(-1)).toBe("\u0003");
    expect(sent).not.toContain("\r");
    expect(sent.join("")).not.toContain("echo");
  });

  it("drops input after dispose", async () => {
    const { sent, guard } = setup();
    guard.dispose();
    guard.input("ls");
    await guard.settled();
    expect(sent).toEqual([]);
  });
});
