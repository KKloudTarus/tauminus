// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from "vitest";

import type { Marker, Screen, ScreenLine } from "./command-guard.ts";
import { CommandRecorder, type RecordedCommand } from "./recorder.ts";

const PROMPT = "user@box:~$ ";

/** A normal-buffer screen the test writes, with markers that follow rows. */
class FakeScreen {
  rows: { text: string; wrapped: boolean }[] = [{ text: "", wrapped: false }];
  cursorX = 0;
  /** The cursor's row, when not the last (moved back with Left or Home). */
  cursorRow: number | undefined;
  type: "normal" | "alternate" = "normal";
  readonly markers: { line: number; dispose(): void }[] = [];

  /** Prints text at the cursor, as the shell's echo or output would. */
  print(text: string): void {
    for (const char of text) {
      if (char === "\n") {
        this.rows.push({ text: "", wrapped: false });
        this.cursorX = 0;
        continue;
      }
      const row = this.rows[this.rows.length - 1]!;
      row.text = row.text.slice(0, this.cursorX) + char + row.text.slice(this.cursorX + 1);
      this.cursorX += 1;
    }
  }

  /** Redraws the cursor's row, as a line editor does on recall or search. */
  redraw(text: string): void {
    const row = this.rows[this.rows.length - 1]!;
    row.text = text;
    this.cursorX = text.length;
  }

  /**
   * Ctrl+L, as xterm.js does it: the rows scroll up out of view and the
   * cursor starts a fresh row; markers keep the rows they were on.
   */
  clear(): void {
    this.rows.push({ text: "", wrapped: false });
    this.cursorX = 0;
  }

  /** Wraps the cursor's row at `width`, as a narrow terminal would. */
  wrap(width: number): void {
    const last = this.rows.pop()!;
    for (let at = 0; at < last.text.length || at === 0; at += width) {
      this.rows.push({ text: last.text.slice(at, at + width), wrapped: at > 0 });
    }
    this.cursorX = this.rows[this.rows.length - 1]!.text.length;
  }

  /**
   * Narrows the terminal, as xterm.js reflows: row `y` splits at `width`,
   * the new rows go after it, and markers below move down with their rows.
   */
  reflow(y: number, width: number): void {
    const row = this.rows[y]!;
    const parts: { text: string; wrapped: boolean }[] = [];
    for (let at = width; at < row.text.length; at += width) {
      parts.push({ text: row.text.slice(at, at + width), wrapped: true });
    }
    row.text = row.text.slice(0, width);
    this.rows.splice(y + 1, 0, ...parts);
    for (const marker of this.markers) if (marker.line > y) marker.line += parts.length;
  }

  /** Drops the oldest `n` rows, as a full scrollback does; their markers go. */
  trim(n: number): void {
    this.rows.splice(0, n);
    for (const marker of this.markers) marker.line = marker.line - n < 0 ? -1 : marker.line - n;
  }

  screen(): Screen {
    const rows = this.rows;
    return {
      type: this.type,
      cursorX: this.cursorX,
      cursorY: this.cursorRow ?? rows.length - 1,
      baseY: 0,
      length: rows.length,
      getLine: (y): ScreenLine | undefined => {
        const row = rows[y];
        if (!row) return undefined;
        return {
          isWrapped: row.wrapped,
          translateToString: (trimRight, start = 0, end = row.text.length) => {
            const text = row.text.slice(start, end);
            return trimRight ? text.trimEnd() : text;
          },
        };
      },
    };
  }

  mark(): Marker {
    const marker = { line: this.cursorRow ?? this.rows.length - 1, dispose: () => {} };
    this.markers.push(marker);
    return marker;
  }
}

function setup(prompt = PROMPT) {
  const screen = new FakeScreen();
  const recorded: RecordedCommand[] = [];
  let now = 1_000;
  const recorder = new CommandRecorder({
    screen: () => screen.screen(),
    mark: () => screen.mark(),
    record: (entry) => {
      recorded.push(entry);
    },
    now: () => now,
  });
  /** The user types `keys` and the shell echoes them. */
  const type = (keys: string) => {
    for (const key of keys) {
      recorder.input(key);
      screen.print(key);
    }
  };
  /** Enter, then the shell prints `output` and a new prompt. */
  const run = (output: string) => {
    recorder.input("\r");
    screen.print(`\n${output}${output ? "\n" : ""}${prompt}`);
  };
  const tick = (ms: number) => {
    now += ms;
  };
  screen.print(prompt);
  const commands = () => recorded.map((r) => r.command);
  return { screen, recorder, recorded, type, run, tick, commands };
}

describe("CommandRecorder", () => {
  it("records each command with the output up to the next prompt", () => {
    const { recorder, recorded, type, run } = setup();
    type("ls -la");
    run("total 0\nfile");
    type("echo hi");
    run("hi");
    expect(recorded).toEqual([
      { command: "ls -la", output: "total 0\nfile", exit: null, startedMs: 1_000 },
    ]);
    recorder.flush();
    expect(recorded[1]).toMatchObject({ command: "echo hi", output: "hi" });
  });

  it("stamps when the command started, not when its output ended", () => {
    const { recorder, recorded, type, run, tick } = setup();
    type("deploy");
    tick(5_000);
    run("done");
    tick(8 * 60 * 60 * 1000);
    recorder.flush();
    expect(recorded[0]?.startedMs).toBe(6_000);
  });

  it("records a recalled command without its prompt", () => {
    const { screen, recorder, commands, run } = setup();
    recorder.input("\u001b[A");
    screen.redraw(`${PROMPT}git status`);
    run("clean");
    recorder.flush();
    expect(commands()).toEqual(["git status"]);
  });

  it("falls back to what follows the prompt when the screen was cleared", () => {
    const { screen, recorder, commands, type, run } = setup();
    type("ls");
    recorder.input("\u000c");
    screen.clear();
    screen.print(`${PROMPT}ls`);
    run("file");
    recorder.flush();
    expect(commands()).toEqual(["ls"]);
  });

  it("records the command a reverse search found", () => {
    const { screen, recorder, commands, run } = setup();
    recorder.input("\u0012");
    for (const key of "ssh") recorder.input(key);
    screen.redraw("(reverse-i-search)`ssh': ssh prod");
    run("");
    recorder.flush();
    expect(commands()).toEqual(["ssh prod"]);
  });

  it("joins continuation lines into one command with the output after the last", () => {
    const { screen, recorder, recorded, type } = setup();
    type("for i in 1 2; do");
    recorder.input("\r");
    screen.print("\n> ");
    type("echo $i");
    recorder.input("\r");
    screen.print("\n> ");
    type("done");
    recorder.input("\r");
    screen.print(`\n1\n2\n${PROMPT}`);
    recorder.flush();
    expect(recorded).toHaveLength(1);
    expect(recorded[0]).toMatchObject({
      command: "for i in 1 2; do\necho $i\ndone",
      output: "1\n2",
    });
  });

  it("takes a pasted command with lines from the paste", () => {
    const { screen, recorder, recorded } = setup();
    recorder.input("\u001b[200~echo a\necho b\u001b[201~");
    screen.print("echo a");
    screen.print("\n> echo b");
    recorder.input("\r");
    screen.print(`\na\nb\n${PROMPT}`);
    recorder.flush();
    expect(recorded[0]).toMatchObject({ command: "echo a\necho b", output: "a\nb" });
  });

  it("records each line of a paste that arrived without brackets", () => {
    const { screen, recorder, commands } = setup();
    recorder.input("echo one\recho two\r");
    screen.print(`echo one\none\n${PROMPT}echo two\ntwo\n${PROMPT}`);
    recorder.flush();
    expect(commands()).toEqual(["echo one", "echo two"]);
  });

  it("ends a command's output at a prompt where Ctrl+C dropped a line", () => {
    const { screen, recorder, recorded, type, run } = setup();
    type("true");
    run("");
    recorder.shellMark("D;0");
    type("foo");
    recorder.input("\u0003");
    screen.print(`^C\n${PROMPT}`);
    // The shell reports the dropped line too; it is not the command's.
    recorder.shellMark("D;130");
    type("pwd");
    run("/home/user");
    recorder.flush();
    expect(recorded[0]).toMatchObject({ command: "true", output: "", exit: 0 });
    expect(recorded.map((r) => r.command)).toEqual(["true", "pwd"]);
  });

  it("records a completed word whatever its case", () => {
    const { screen, recorder, commands, type, run } = setup();
    type("cd doc");
    recorder.input("\t");
    screen.redraw(`${PROMPT}cd Documents/`);
    run("");
    recorder.flush();
    expect(commands()).toEqual(["cd Documents/"]);
  });

  it("leaves a right prompt out of the command", () => {
    const { screen, recorder, commands, type, run } = setup();
    type("ls");
    screen.redraw(`${PROMPT}ls${" ".repeat(30)}[10:00]`);
    run("");
    recorder.flush();
    expect(commands()).toEqual(["ls"]);
  });

  it("records nothing typed at a REPL's prompt", () => {
    const { screen, recorder, commands, type } = setup();
    type("python3");
    recorder.input("\r");
    screen.print("\nPython 3.12\n>>> ");
    type("token = 'abc'");
    recorder.input("\r");
    screen.print("\n>>> ");
    type("exit()");
    recorder.input("\r");
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    expect(commands()).toEqual(["python3"]);
  });

  it("records nothing that starts with a space, as bash's ignorespace", () => {
    const { recorder, commands, type, run } = setup();
    type(" export TOKEN=abc");
    run("");
    type("ls");
    run("");
    recorder.flush();
    expect(commands()).toEqual(["ls"]);
  });

  it("records nothing at a secret prompt or when the keys do not echo", () => {
    const { screen, recorder, recorded, type } = setup();
    type("sudo id");
    recorder.input("\r");
    screen.print("\n[sudo] password for alice: ");
    for (const key of "hunter2") recorder.input(key);
    recorder.input("\r");
    screen.print("\nuid=0(root)\n");
    // A prompt for a key, which does echo.
    screen.print("Enter API key: ");
    type("sk-live-abc");
    recorder.input("\r");
    screen.print(`\n${PROMPT}`);
    // No word to give it away, and echo off.
    type("read -s x");
    recorder.input("\r");
    screen.print("\n");
    for (const key of "s3cret") recorder.input(key);
    recorder.input("\r");
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    const text = JSON.stringify(recorded);
    for (const secret of ["hunter2", "sk-live-abc", "s3cret"]) {
      expect(text).not.toContain(secret);
    }
    expect(recorded.map((r) => r.command)).toEqual(["sudo id", "read -s x"]);
    expect(recorded[0]?.output).toContain("uid=0(root)");
    // The echoed secret's line keeps its prompt only.
    expect(recorded[0]?.output).toContain("Enter API key: •••");
  });

  it("records nothing typed in a full-screen program", () => {
    const { screen, recorder, commands, type, run } = setup();
    screen.type = "alternate";
    for (const key of ":wq") recorder.input(key);
    recorder.input("\r");
    // The program exits; the shell's prompt is back as it was.
    screen.type = "normal";
    type("ls");
    run("");
    recorder.flush();
    expect(commands()).toEqual(["ls"]);
  });

  it("uses the shell's own marks when it sends them", () => {
    const { screen, recorder, recorded, type } = setup("");
    recorder.shellMark("A");
    screen.print("❯ ");
    recorder.shellMark("B");
    type("make test");
    recorder.input("\r");
    screen.print("\n");
    recorder.shellMark("C");
    screen.print("ok\nFAILED 1\n");
    recorder.shellMark("D;2");
    recorder.shellMark("A");
    screen.print("❯ ");
    expect(recorder.integrated).toBe(true);
    expect(recorded).toEqual([
      { command: "make test", output: "ok\nFAILED 1", exit: 2, startedMs: 1_000 },
    ]);
  });

  it("an empty Enter ends the output of the command before it", () => {
    const { recorder, recorded, type, run } = setup();
    type("date");
    run("Thu");
    recorder.input("\r");
    expect(recorded).toEqual([{ command: "date", output: "Thu", exit: null, startedMs: 1_000 }]);
  });

  it("ignores replies the terminal sends while a command runs", () => {
    const { screen, recorder, commands, type } = setup();
    type("bat file");
    recorder.input("\r");
    screen.print("\n");
    recorder.input("\u001b]11;rgb:0000/0000/0000\u001b\\");
    recorder.input("\u001b[I");
    screen.print(`contents\n${PROMPT}`);
    type("ls");
    recorder.input("\r");
    screen.print(`\nfile\n${PROMPT}`);
    recorder.flush();
    expect(commands()).toEqual(["bat file", "ls"]);
  });

  it("keeps answers to a running program out of commands and output", () => {
    const { screen, recorder, recorded, type } = setup();
    type("apt install foo");
    recorder.input("\r");
    screen.print("\nDo you want to continue? [Y/n] ");
    type("y");
    recorder.input("\r");
    screen.print("\nunpacking...\n");
    type("cat > notes");
    recorder.input("\r");
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    expect(recorded.map((r) => r.command)).toEqual(["apt install foo"]);
    expect(recorded[0]?.output).toBe("Do you want to continue? [Y/n] •••\nunpacking...\n•••");
  });

  it("never records a password pasted with a newline at a hidden prompt", () => {
    const { screen, recorder, recorded, type } = setup();
    type("sudo id");
    recorder.input("\r");
    screen.print("\n[sudo] password for alice: ");
    recorder.input("hunter2\r");
    screen.print("\nuid=0(root)\n");
    type("ls");
    recorder.input("\r");
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    expect(JSON.stringify(recorded)).not.toContain("hunter2");
    expect(recorded.map((r) => r.command)).toEqual(["sudo id"]);
  });

  it("cuts every row of an echoed secret that wrapped", () => {
    const { screen, recorder, recorded, type } = setup();
    type("aws configure");
    recorder.input("\r");
    screen.print("\nAWS Secret Access Key [None]: ");
    type("wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEYzz");
    screen.wrap(40);
    recorder.input("\r");
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    expect(recorded[0]?.output).toBe("AWS Secret Access Key [None]: •••");
  });

  it("does not take a token typed under a prompt without a keyword for a command", () => {
    const { screen, recorder, recorded, type } = setup();
    type("./setup.sh");
    recorder.input("\r");
    screen.print("\nPaste your deploy token below\n");
    type("ghp_abcdef123456");
    recorder.input("\r");
    screen.print(`\ndone\n${PROMPT}`);
    recorder.flush();
    expect(JSON.stringify(recorded)).not.toContain("ghp_abcdef123456");
    expect(recorded.map((r) => r.command)).toEqual(["./setup.sh"]);
  });

  it("with shell marks, input typed while a command runs is not a command", () => {
    const { screen, recorder, recorded, type } = setup("");
    recorder.shellMark("A");
    screen.print("$ ");
    recorder.shellMark("B");
    type("sleep 5");
    recorder.input("\r");
    screen.print("\n");
    recorder.shellMark("C");
    type("ls");
    recorder.input("\r");
    screen.print("\n");
    recorder.shellMark("D;0");
    recorder.shellMark("A");
    expect(recorded).toEqual([{ command: "sleep 5", output: "•••", exit: 0, startedMs: 1_000 }]);
  });

  it("Ctrl+C at an empty prompt ends the output of the command before it", () => {
    const { screen, recorder, recorded, type, run } = setup();
    type("ls");
    run("f");
    recorder.input("\u0003");
    screen.print(`^C\n${PROMPT}`);
    type("pwd");
    run("/");
    expect(recorded[0]).toMatchObject({ command: "ls", output: "f" });
  });

  it("keeps lines typed into a program with a `>` prompt out of commands", () => {
    for (const [program, banner, prompt, line] of [
      ["node", "Welcome to Node.js", "> ", "process.env.TOKEN"],
      ["sftp prod", "Connected to prod.", "sftp> ", "put secret.txt"],
      ["redis-cli -h db", "", "db:6379> ", "AUTH hunter2"],
      ["./activate", "", "Enter license key> ", "LIC-12345-SECRET"],
    ] as const) {
      const { screen, recorder, recorded, type } = setup();
      type(program);
      recorder.input("\r");
      screen.print(`\n${banner}${banner ? "\n" : ""}${prompt}`);
      type(line);
      recorder.input("\r");
      screen.print(`\n${prompt}`);
      type(".exit");
      recorder.input("\r");
      screen.print(`\n${PROMPT}`);
      recorder.flush();
      expect({ program, commands: recorded.map((r) => r.command) }).toEqual({
        program,
        commands: [program],
      });
      expect(JSON.stringify(recorded)).not.toContain(line);
    }
  });

  it("with shell marks, joins a command's continuation lines", () => {
    for (const secondary of [undefined, "A;k=s"]) {
      const { screen, recorder, recorded, type } = setup("");
      recorder.shellMark("A");
      screen.print("$ ");
      recorder.shellMark("B");
      type("echo 'a");
      recorder.input("\r");
      screen.print("\n");
      if (secondary) recorder.shellMark(secondary);
      screen.print("> ");
      if (secondary) recorder.shellMark("B");
      type("b'");
      recorder.input("\r");
      screen.print("\n");
      recorder.shellMark("C");
      screen.print("a\nb\n");
      recorder.shellMark("D;0");
      recorder.shellMark("A");
      expect({ secondary, recorded }).toMatchObject({
        secondary,
        recorded: [{ command: "echo 'a\nb'", output: "a\nb", exit: 0, startedMs: 1_000 }],
      });
    }
  });

  it("cuts a secret's rows that wrapped when the terminal narrowed", () => {
    const { screen, recorder, recorded, type } = setup();
    type("aws configure");
    recorder.input("\r");
    screen.print("\nAWS Secret Access Key [None]: ");
    type("wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY");
    recorder.input("\r");
    const secretRow = screen.rows.length - 1;
    screen.print("\nDefault region name [None]: ");
    screen.reflow(secretRow, 30);
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    expect(JSON.stringify(recorded)).not.toContain("EXAMPLEKEY");
    expect(recorded[0]?.output).toBe(
      "AWS Secret Access Key [None]: •••\nDefault region name [None]:",
    );
  });

  it("cuts a wrapped secret when Enter came with the cursor on its first row", () => {
    const { screen, recorder, recorded, type } = setup();
    type("read -e -p 'Value for API: ' v");
    recorder.input("\r");
    screen.print("\nValue for API: ");
    type("wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEYzzzz");
    screen.wrap(30);
    screen.cursorRow = screen.rows.length - 2;
    recorder.input("\u001b[H");
    recorder.input("\r");
    screen.cursorRow = undefined;
    screen.print(`\n${PROMPT}`);
    recorder.flush();
    expect(JSON.stringify(recorded)).not.toContain("EXAMPLEKEY");
  });

  it("cuts a wrapped secret whose first row left the scrollback", () => {
    const { screen, recorder, recorded, type } = setup();
    type("aws configure");
    recorder.input("\r");
    screen.print("\nAWS Secret Access Key [None]: ");
    type("wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEYTAILTAILTAIL");
    screen.wrap(30);
    recorder.input("\r");
    screen.print("\nok1\nok2\n");
    // Everything up to the secret's first row scrolled away.
    screen.trim(screen.rows.findIndex((r) => r.text.startsWith("AWS")) + 1);
    screen.print(PROMPT);
    recorder.flush();
    expect(JSON.stringify(recorded)).not.toContain("TAIL");
    expect(recorded[0]?.output).toBe("ok1\nok2");
  });

  it("goes on recording when the prompt changes after a venv or a cd", () => {
    const { screen, recorder, commands, type } = setup("~/src$ ");
    type("source .venv/bin/activate");
    recorder.input("\r");
    screen.print("\n(.venv) ~/src$ ");
    type("cd /etc");
    recorder.input("\r");
    screen.print("\n(.venv) /etc$ ");
    type("pytest");
    recorder.input("\r");
    screen.print("\n3 passed\n(.venv) /etc$ ");
    recorder.flush();
    expect(commands()).toEqual(["source .venv/bin/activate", "cd /etc", "pytest"]);
  });

  it("reads the screen when the shell marks prompts with A only", () => {
    const { screen, recorder, commands, type } = setup("");
    for (const command of ["ls", "pwd"]) {
      recorder.shellMark("A");
      screen.print("$ ");
      type(command);
      recorder.input("\r");
      screen.print("\nout\n");
    }
    recorder.shellMark("A");
    expect(commands()).toEqual(["ls", "pwd"]);
  });

  it("with A and B but no C, lines typed into a REPL are not the command", () => {
    const { screen, recorder, recorded, type } = setup("");
    recorder.shellMark("A");
    screen.print("$ ");
    recorder.shellMark("B");
    type("python3");
    recorder.input("\r");
    screen.print("\n>>> ");
    type("db.connect(password='hunter2')");
    recorder.input("\r");
    screen.print("\n>>> ");
    type("exit()");
    recorder.input("\r");
    screen.print("\n");
    recorder.shellMark("A");
    expect(recorded.map((r) => r.command)).toEqual(["python3"]);
    expect(JSON.stringify(recorded)).not.toContain("hunter2");
  });
});
