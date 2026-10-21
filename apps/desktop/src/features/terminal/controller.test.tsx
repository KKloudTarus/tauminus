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

import { ACK_THRESHOLD_BYTES } from "./ack-batcher.ts";
import { TerminalController } from "./controller.ts";
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

function setup() {
  const sessions = new FakeSessions();
  const controller = new TerminalController(sessions);
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
    await expect
      .poll(() => sessions.session.acknowledge.mock.calls)
      .toEqual([[ACK_THRESHOLD_BYTES + 5]]);
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
