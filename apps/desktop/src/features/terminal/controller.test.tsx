// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { render } from "vitest-browser-react";

import {
  type GridSize,
  type SessionClient,
  SessionCommandError,
  type SessionEvents,
  type SessionSpec,
  type TerminalSession,
} from "@/ipc/index.ts";

// The app's styles, so views get the layout they have in the app; without
// them a pane takes its size from the terminal inside it.
import "@/app/styles.css";

import { ACK_THRESHOLD_BYTES } from "./ack-batcher.ts";
import { TerminalController } from "./controller.ts";
import { OutputScheduler } from "./output-scheduler.ts";
import { TerminalView } from "./TerminalView.tsx";

/** A session client the test drives by hand. */
class FakeSessions implements SessionClient {
  opened: { spec: SessionSpec; size: GridSize; events: SessionEvents }[] = [];
  session = {
    id: 1,
    write: vi.fn<TerminalSession["write"]>(async () => {}),
    writeBytes: vi.fn<TerminalSession["writeBytes"]>(async () => {}),
    resize: vi.fn<TerminalSession["resize"]>(async () => {}),
    acknowledge: vi.fn<TerminalSession["acknowledge"]>(async () => {}),
    close: vi.fn<TerminalSession["close"]>(async () => {}),
  } satisfies TerminalSession;
  #resolve: ((session: TerminalSession) => void) | undefined;
  #reject: ((error: unknown) => void) | undefined;

  open(spec: SessionSpec, size: GridSize, events: SessionEvents): Promise<TerminalSession> {
    this.opened.push({ spec, size, events });
    return new Promise((resolve, reject) => {
      this.#resolve = resolve;
      this.#reject = reject;
    });
  }

  resolveOpen() {
    this.#resolve?.(this.session);
  }

  rejectOpen(error: unknown) {
    this.#reject?.(error);
  }

  get events(): SessionEvents {
    const latest = this.opened.at(-1);
    if (!latest) throw new Error("no session opened");
    return latest.events;
  }
}

const bytes = (length: number) => new Uint8Array(length).fill(0x61);
const controllers: TerminalController[] = [];

function setup(options: ConstructorParameters<typeof TerminalController>[1] = {}) {
  const sessions = new FakeSessions();
  const controller = new TerminalController(sessions, options);
  controllers.push(controller);
  return { sessions, controller };
}

async function mount(controller: TerminalController) {
  return render(<TerminalView controller={controller} className="h-[300px] w-[600px]" />);
}

afterEach(() => {
  for (const controller of controllers.splice(0)) {
    controller.dispose();
  }
});

describe("TerminalController with TerminalView", () => {
  it("opens one session sized to the terminal grid on first attach", async () => {
    const { sessions, controller } = setup();
    expect(sessions.opened).toHaveLength(0);
    await mount(controller);
    expect(sessions.opened).toHaveLength(1);
    const { size } = sessions.opened[0]!;
    expect(size.cols).toBeGreaterThan(10);
    expect(size.rows).toBeGreaterThan(3);
  });

  it("keeps the session and screen when the view remounts elsewhere", async () => {
    const { sessions, controller } = setup();
    const first = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    controller.terminal.write("kept-on-screen");
    await first.unmount();

    const second = await mount(controller);
    expect(sessions.opened).toHaveLength(1);
    expect(sessions.session.close).not.toHaveBeenCalled();
    expect(second.container.contains(controller.terminal.element!)).toBe(true);
    const line = controller.terminal.buffer.active.getLine(0)?.translateToString(true);
    expect(line).toBe("kept-on-screen");
  });

  it("tells the session the new size when the view remounts into a smaller host", async () => {
    // Inline sizes: these tests run without the app's CSS.
    const sized = (width: number) => (
      <div style={{ width, height: 300 }}>
        <TerminalView controller={controller} />
      </div>
    );
    const { sessions, controller } = setup();
    const first = await render(sized(600));
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    const before = sessions.opened[0]!.size;
    await first.unmount();
    // A split halves the pane: the view remounts at half the width.
    await render(sized(300));
    await expect
      .poll(() => sessions.session.resize.mock.calls.at(-1)?.[0].cols ?? before.cols)
      .toBeLessThan(before.cols);
    // The same size again is not sent twice.
    const calls = sessions.session.resize.mock.calls.length;
    controller.attach(controller.terminal.element!.parentElement!);
    expect(sessions.session.resize.mock.calls.length).toBe(calls);
  });

  it("does not start a new session when the view remounts after the session ended", async () => {
    const { sessions, controller } = setup();
    const first = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    sessions.events.onExit(3);
    await first.unmount();
    // Closing a neighbouring pane remounts this one.
    await mount(controller);
    expect(sessions.opened).toHaveLength(1);
    expect(controller.status).toEqual({ kind: "exited", code: 3 });
  });

  it("sends typed input to the session in order", async () => {
    const { sessions, controller } = setup();
    const screen = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    await screen.getByTestId("terminal-host").click();
    await userEvent.keyboard("ls -la");
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data).join(""))
      .toBe("ls -la");
  });

  it("sends Ctrl+C at once and tells a listener what the user typed", async () => {
    const { sessions, controller } = setup();
    const view = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    const heard: string[] = [];
    controller.setInputListener((data) => heard.push(data));
    await view.getByTestId("terminal-host").click();
    await userEvent.keyboard("ab{Control>}c{/Control}");
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data).join(""))
      .toBe("ab\u0003");
    expect(heard.join("")).toBe("ab\u0003");
  });

  it("brackets a shared paste only when its own shell asked for it", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    const written = () => sessions.session.write.mock.calls.map(([data]) => data).join("");
    controller.typeShared("\u001b[200~a\u001b[201~");
    await expect.poll(written).toBe("a");
    controller.terminal.write("\u001b[?2004h");
    await vi.waitFor(() => expect(controller.terminal.modes.bracketedPasteMode).toBe(true));
    controller.typeShared("\u001b[200~b\u001b[201~");
    await expect.poll(written).toBe("a\u001b[200~b\u001b[201~");
  });

  it("types a snippet as keys on a host without a guard", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    controller.type("uptime\r");
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data).join(""))
      .toBe("uptime\r");
  });

  it("removes escapes from inside a bracketed paste before the shell sees it", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    // The shell turns bracketed paste on, as bash and zsh do.
    controller.terminal.write("\u001b[?2004h");
    await vi.waitFor(() => expect(controller.terminal.modes.bracketedPasteMode).toBe(true));
    controller.terminal.paste("echo a\u001b[201~ rm -rf ~");
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data))
      .toContain("\u001b[200~echo a[201~ rm -rf ~\u001b[201~");
  });

  it("acknowledges output once xterm.js has parsed it", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));

    sessions.events.onOutput(bytes(ACK_THRESHOLD_BYTES));
    await expect
      .poll(() => sessions.session.acknowledge.mock.calls)
      .toEqual([[ACK_THRESHOLD_BYTES]]);
  });

  it("acknowledges output that arrived before the session finished opening", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.events.onOutput(bytes(ACK_THRESHOLD_BYTES + 5));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sessions.session.acknowledge).not.toHaveBeenCalled();

    sessions.resolveOpen();
    // Output reaches xterm.js in slices, so the ack covers the first
    // threshold's worth; the 5 bytes left over stay below the threshold.
    await expect
      .poll(() => sessions.session.acknowledge.mock.calls)
      .toEqual([[ACK_THRESHOLD_BYTES]]);
  });

  it("never lets the terminal's own width size its pane", async () => {
    // A pane beside another in a row, as a split lays them out. If wide
    // content could push the pane wider, fit would add columns, the content
    // would widen again, and on macOS that loop ran 30 times a second.
    const { sessions, controller } = setup();
    const screen = await render(
      <div style={{ display: "flex", width: 600, height: 300 }}>
        <TerminalView controller={controller} className="flex-1" />
        <div style={{ flex: 1 }} />
      </div>,
    );
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    const host = screen.getByTestId("terminal-host").element();
    const before = host.getBoundingClientRect().width;
    controller.terminal.resize(400, 24);
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    expect(host.getBoundingClientRect().width).toBe(before);
    expect(before).toBeLessThanOrEqual(300);
  });

  it("answers a cursor query that arrives before the session finishes opening", async () => {
    // Windows' ConPTY asks for the cursor position at startup and holds the
    // shell until the answer comes; it can come before `open` resolves.
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.events.onOutput(new TextEncoder().encode("\u001b[6n"));
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(sessions.session.write).not.toHaveBeenCalled();
    sessions.resolveOpen();
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data.replace("\u001b", "ESC")))
      .toEqual([expect.stringMatching(/^ESC\[\d+;\d+R$/)]);
  });

  it("drops input typed before a session that fails to open", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    controller.terminal.input("ls\r");
    sessions.rejectOpen(new SessionCommandError({ code: "launch_failed", message: "no shell" }));
    await vi.waitFor(() => expect(controller.status.kind).toBe("failed"));
    expect(sessions.session.write).not.toHaveBeenCalled();
  });

  it("keeps other panes' output flowing when a pane closes in the middle of its turn", async () => {
    // A watchdog too slow to matter: only closing can hand the turn on.
    const output = new OutputScheduler({ slice: 4096, direct: 4096, turnMs: 6, stallMs: 60_000 });
    const first = setup({ output });
    const second = setup({ output });
    await mount(first.controller);
    await mount(second.controller);
    first.sessions.resolveOpen();
    second.sessions.resolveOpen();
    first.sessions.events.onOutput(bytes(4 * ACK_THRESHOLD_BYTES));
    second.sessions.events.onOutput(bytes(ACK_THRESHOLD_BYTES));
    const writes = vi.spyOn(first.controller.terminal, "write");
    first.controller.dispose();
    await expect
      .poll(() => second.sessions.session.acknowledge.mock.calls)
      .toEqual([[ACK_THRESHOLD_BYTES]]);
    // What the closed pane still had queued never reaches its terminal.
    expect(writes).not.toHaveBeenCalled();
  });

  it("drops a repeat count that would hang the terminal and keeps parsing", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    // Padded past the direct-write size so it takes a scheduled turn.
    const payload = `${"x".repeat(5000)}\r\nA\u001b[2147483647b\r\nafter\u001b[3b`;
    sessions.events.onOutput(new TextEncoder().encode(payload));
    await expect
      .poll(() => {
        const { buffer } = controller.terminal;
        const last = buffer.active.getLine(buffer.active.baseY + buffer.active.cursorY);
        return last?.translateToString(true);
      })
      .toBe("afterrrr");
  });

  it("shows the exit code when the shell ends", async () => {
    const { sessions, controller } = setup();
    const screen = await mount(controller);
    sessions.resolveOpen();
    sessions.events.onExit(3);
    await expect
      .element(screen.getByRole("status"))
      .toHaveTextContent("Process exited with code 3");
  });

  it("shows why the shell failed to start", async () => {
    const { sessions, controller } = setup();
    const screen = await mount(controller);
    sessions.rejectOpen(
      new SessionCommandError({ code: "launch_failed", message: "no such shell" }),
    );
    await expect
      .element(screen.getByRole("status"))
      .toHaveTextContent("Could not start the shell: no such shell");
  });

  it("opens what it was created with", async () => {
    const sessions = new FakeSessions();
    const spec: SessionSpec = {
      kind: "ssh",
      target: { destination: { user: "a", host: "db", port: 2222 }, route: { kind: "direct" } },
    };
    const controller = new TerminalController(sessions, { spec });
    controllers.push(controller);
    expect(controller.defaultTitle).toBe("a@db:2222");
    await mount(controller);
    expect(sessions.opened[0]?.spec).toBe(spec);
  });

  it("shows prompts one at a time and returns each answer", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    const ask = sessions.events.onPrompt;
    if (!ask) throw new Error("no prompt handler");
    const first = ask({ kind: "password", endpoint: "a@db" });
    const second = ask({ kind: "passphrase", keyPath: "/k" });
    expect(controller.getPrompt()?.kind).toBe("password");
    controller.answer({ kind: "secret", value: "pw" });
    await expect(first).resolves.toEqual({ kind: "secret", value: "pw" });
    expect(controller.getPrompt()?.kind).toBe("passphrase");
    controller.answer({ kind: "cancel" });
    await expect(second).resolves.toEqual({ kind: "cancel" });
    expect(controller.getPrompt()).toBeUndefined();
  });

  it("cancels waiting prompts when disposed or when the connect fails", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    const pending = sessions.events.onPrompt?.({ kind: "password", endpoint: "a@db" });
    sessions.rejectOpen(new SessionCommandError({ code: "connect_failed", message: "down" }));
    await expect(pending).resolves.toEqual({ kind: "cancel" });
    expect(controller.getPrompt()).toBeUndefined();

    controller.retry();
    const later = sessions.events.onPrompt?.({ kind: "password", endpoint: "a@db" });
    controller.dispose();
    await expect(later).resolves.toEqual({ kind: "cancel" });
  });

  it("retries after a failed start and only then", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    controller.retry();
    expect(sessions.opened).toHaveLength(1);
    sessions.rejectOpen(new SessionCommandError({ code: "auth_failed", message: "no" }));
    await vi.waitFor(() =>
      expect(controller.status).toEqual({ kind: "failed", message: "no", code: "auth_failed" }),
    );
    controller.retry();
    expect(controller.status.kind).toBe("starting");
    expect(sessions.opened).toHaveLength(2);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
  });

  it("puts the prompt dialog over the pane and answers from it", async () => {
    const sessions = new FakeSessions();
    const controller = new TerminalController(sessions, {
      spec: {
        kind: "ssh",
        target: { destination: { user: "a", host: "db", port: 22 }, route: { kind: "direct" } },
      },
    });
    controllers.push(controller);
    const screen = await mount(controller);
    const answer = sessions.events.onPrompt?.({
      kind: "unknownHostKey",
      host: "db",
      port: 22,
      algorithm: "ssh-ed25519",
      fingerprint: "SHA256:abc",
      certificateExpected: true,
    });
    await expect.element(screen.getByRole("dialog", { name: "New host: db" })).toBeVisible();
    await expect.element(screen.getByText("SHA256:abc")).toBeVisible();
    await expect.element(screen.getByText(/did not present a certificate/)).toBeVisible();
    await expect.element(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    // Shortcuts that refocus the active pane must not pull focus from the prompt.
    controller.focus();
    await expect.element(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    // A DOM click: these tests run without the app's CSS, so the overlay is
    // in normal flow and never "stable" for a pointer click. The end-to-end
    // test clicks it for real.
    const trust = screen.getByRole("button", { name: "Trust and remember" }).element();
    if (!(trust instanceof HTMLElement)) throw new Error("not a button");
    trust.click();
    await expect(answer).resolves.toEqual({ kind: "trustHostKey", remember: true });
    await expect.element(screen.getByRole("dialog")).not.toBeInTheDocument();
  });

  it("explains a changed host key and offers no retry", async () => {
    const sessions = new FakeSessions();
    const controller = new TerminalController(sessions, {
      spec: {
        kind: "ssh",
        target: { destination: { user: "a", host: "db", port: 22 }, route: { kind: "direct" } },
      },
    });
    controllers.push(controller);
    const screen = await mount(controller);
    sessions.rejectOpen(
      new SessionCommandError({ code: "host_key_changed", message: "recorded at kh:3" }),
    );
    await expect
      .element(screen.getByRole("heading", { name: "Warning: the host key for a@db has changed" }))
      .toBeVisible();
    await expect.element(screen.getByText("recorded at kh:3")).toBeVisible();
    expect(screen.getByRole("button", { name: "Try again" }).query()).toBeNull();
  });

  it("closes the session on dispose, not on unmount", async () => {
    const { sessions, controller } = setup();
    const screen = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    await screen.unmount();
    expect(sessions.session.close).not.toHaveBeenCalled();
    controller.dispose();
    expect(sessions.session.close).toHaveBeenCalledOnce();
  });

  it("closes a session that finishes opening after dispose", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    controller.dispose();
    sessions.resolveOpen();
    await expect.poll(() => sessions.session.close.mock.calls.length).toBe(1);
  });

  it("retries an acknowledgement whose call failed instead of losing it", async () => {
    const { sessions, controller } = setup();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    sessions.session.acknowledge.mockRejectedValueOnce(new Error("ipc down"));
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    sessions.events.onOutput(bytes(ACK_THRESHOLD_BYTES));
    await expect
      .poll(() => sessions.session.acknowledge.mock.calls)
      .toEqual([[ACK_THRESHOLD_BYTES], [ACK_THRESHOLD_BYTES]]);
    consoleError.mockRestore();
  });

  it(
    "keeps rendering on the DOM renderer after the WebGL context is lost",
    { timeout: 10_000 },
    async () => {
      const { sessions, controller } = setup();
      const screen = await mount(controller);
      sessions.resolveOpen();
      // The addon loads on demand; headless Chromium provides WebGL2 via SwiftShader.
      await expect.poll(() => controller.rendererKind).toBe("webgl");
      const loseContext = [...screen.container.querySelectorAll("canvas")]
        .map((canvas) => canvas.getContext("webgl2")?.getExtension("WEBGL_lose_context"))
        .find((extension) => extension);
      expect(loseContext).toBeDefined();
      loseContext?.loseContext();
      // The addon waits 3 s for the context to come back before giving it up.
      await expect.poll(() => controller.rendererKind, { timeout: 5000 }).toBe("dom");

      controller.terminal.write("after-context-loss");
      await expect
        .poll(() => screen.container.querySelector(".xterm-rows")?.textContent ?? "")
        .toContain("after-context-loss");
    },
  );
});

function screenText(controller: TerminalController): string {
  return controller.terminal.buffer.active.getLine(0)?.translateToString(true) ?? "";
}

describe("TerminalController with the prod guard", () => {
  const HOST: SessionSpec = {
    kind: "host",
    hostId: "db",
    label: "db-prod",
    target: {
      destination: { user: "ops", host: "db", port: 22 },
      route: { kind: "direct" },
    },
  };

  function guarded() {
    const checked: string[] = [];
    let listener: (() => void) | undefined;
    const source = {
      on: true,
      active: () => source.on,
      subscribe: (next: () => void) => {
        listener = next;
        return () => {
          listener = undefined;
        };
      },
      check: async (lines: string[]) => {
        checked.push(...lines);
        const line = lines.find((l) => l.includes("rm -rf"));
        return {
          findings: line
            ? [{ rule: "recursive-delete", reason: "deletes files recursively", command: line }]
            : [],
          incomplete: false,
        };
      },
      toggle: (on: boolean) => {
        source.on = on;
        listener?.();
      },
    };
    const { sessions, controller } = setup({ spec: HOST, guard: source });
    // The shell echoes what it is sent, as a remote one does.
    sessions.session.write.mockImplementation(async (data) => {
      controller.terminal.write(data === "\r" ? "\r\n$ " : data);
    });
    return { sessions, controller, source, checked };
  }

  it("asks before Enter runs a dangerous line, and sends it only on Run", async () => {
    const { sessions, controller, checked } = guarded();
    const view = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    controller.focus();
    await userEvent.keyboard("rm -rf /srv");
    await expect.poll(() => screenText(controller)).toBe("rm -rf /srv");
    await userEvent.keyboard("{Enter}");

    const dialog = view.getByTestId("guard-dialog");
    await expect.element(dialog).toBeVisible();
    await expect.element(view.getByTestId("guard-text")).toHaveTextContent("rm -rf /srv");
    await expect.element(view.getByRole("button", { name: "Cancel" })).toHaveFocus();
    expect(checked).toEqual(["rm -rf /srv"]);
    expect(sessions.session.write.mock.calls.map(([data]) => data)).not.toContain("\r");

    // A reflexive Enter lands on Cancel.
    await userEvent.keyboard("{Enter}");
    await expect.element(dialog).not.toBeInTheDocument();
    expect(sessions.session.write.mock.calls.map(([data]) => data)).not.toContain("\r");

    controller.focus();
    await userEvent.keyboard("{Enter}");
    await view.getByRole("button", { name: "Run" }).click();
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data))
      .toContain("\r");
  });

  it("shows the prod frame while the host is guarded", async () => {
    const { controller, source } = guarded();
    const view = await mount(controller);
    await expect
      .poll(() => view.getByTestId("prod-status").query()?.textContent)
      .toBe("PROD · db-prod · commands are checked before they run");
    source.toggle(false);
    await expect.element(view.getByTestId("prod-status")).not.toBeInTheDocument();
  });

  it("asks before a multi-line paste", async () => {
    const { sessions, controller } = guarded();
    const view = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    controller.terminal.paste("echo a\necho b\n");
    await expect
      .poll(() => view.getByRole("heading").query()?.textContent)
      .toBe("Paste 2 lines into db-prod?");
    await view.getByRole("button", { name: "Paste" }).click();
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data))
      .toContain("echo a\recho b\r");
  });

  it("asks before a snippet typed into it runs", async () => {
    const { sessions, controller } = guarded();
    const view = await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    controller.type("systemctl restart nginx\r");
    await expect
      .poll(() => view.getByRole("heading").query()?.textContent)
      .toBe("Paste 1 line into db-prod?");
    expect(sessions.session.write.mock.calls.map(([data]) => data)).toEqual([]);
    await view.getByRole("button", { name: "Paste" }).click();
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data))
      .toContain("systemctl restart nginx\r");
  });

  it("sends Ctrl+C at once while a check holds input", async () => {
    const { sessions, controller, source } = guarded();
    source.check = () => new Promise(() => {});
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    // A pasted line waits on its check; what follows waits behind it.
    controller.type("ls\r");
    controller.type("pwd");
    controller.interrupt();
    await expect
      .poll(() => sessions.session.write.mock.calls.map(([data]) => data).join(""))
      .toBe("\u0003");
  });

  it("answers no to a pending question when disposed", async () => {
    const { sessions, controller } = guarded();
    await mount(controller);
    sessions.resolveOpen();
    await vi.waitFor(() => expect(controller.status.kind).toBe("running"));
    controller.terminal.paste("a\nb\n");
    await expect.poll(() => controller.getGuardRequest()).toBeDefined();
    controller.dispose();
    expect(controller.getGuardRequest()).toBeUndefined();
  });
});
