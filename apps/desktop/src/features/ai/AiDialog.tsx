// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { type FormEvent, useEffect, useId, useRef, useState } from "react";

import { messageOf } from "@/features/hosts/hosts-store.ts";
import {
  type AiClient,
  type AiReplyView,
  type AiSettingsView,
  SessionCommandError,
} from "@/ipc/index.ts";

const FIELD =
  "rounded border border-current/20 bg-transparent px-2 py-1.5 outline-none focus:border-current/50";
const BUTTON =
  "rounded border border-current/20 px-3 py-1.5 hover:bg-current/10 disabled:opacity-50";
const PRIMARY =
  "rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90 disabled:opacity-50";
const ALERT = "text-red-600 dark:text-red-400";

export type AssistantMode = "ask" | "explain" | "settings";

interface AiDialogProps {
  client: AiClient;
  mode: AssistantMode;
  /** The pane's session; `undefined` when no pane has one. */
  session: number | undefined;
  onClose: () => void;
  /** Puts a command on the pane's line without Enter; `false` if it could not. */
  onInsert: (command: string) => boolean;
  onCopy: (text: string) => void;
  /** Opens the vault, then runs `retry`. */
  onUnlock: (retry: () => void) => void;
}

type Step =
  | { kind: "idle" }
  | { kind: "busy"; what: string }
  | { kind: "preview"; preview: number; text: string }
  | { kind: "reply"; reply: AiReplyView }
  | { kind: "failed"; text: string };

/**
 * The AI assistant: a command for what the user asks, or what went wrong in
 * the pane's output. Nothing it answers runs: a command goes on the line,
 * without Enter, for the user to read. A modal dialog.
 */
export function AiDialog({
  client,
  mode: initial,
  session,
  onClose,
  onInsert,
  onCopy,
  onUnlock,
}: AiDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [settings, setSettings] = useState<AiSettingsView | undefined>();
  const [mode, setMode] = useState<AssistantMode>(initial);
  const [request, setRequest] = useState("");
  const [step, setStep] = useState<Step>({ kind: "idle" });
  const [inserted, setInserted] = useState<"yes" | "no" | undefined>();

  useEffect(() => {
    const dialog = dialogRef.current;
    if (dialog && !dialog.open) dialog.showModal();
    let current = true;
    client
      .settings()
      .then((loaded) => {
        if (!current) return;
        setSettings(loaded);
        if (!loaded.enabled) setMode("settings");
      })
      .catch((error: unknown) => {
        if (current) setStep({ kind: "failed", text: messageOf(error) });
      });
    return () => {
      current = false;
    };
  }, [client]);

  const run = (what: string, work: () => Promise<Step>) => {
    setInserted(undefined);
    runStep(what, work, setStep, onUnlock);
  };

  // Explaining starts by showing what would be sent.
  const previewed = useRef(false);
  useEffect(() => {
    if (mode !== "explain" || !settings?.enabled || session === undefined || previewed.current) {
      return;
    }
    previewed.current = true;
    runStep(
      "Reading the pane's output…",
      async () => {
        const preview = await client.preview(session);
        return { kind: "preview", preview: preview.preview, text: preview.text };
      },
      setStep,
      onUnlock,
    );
  }, [mode, settings, session, client, onUnlock]);

  const ask = (event: FormEvent) => {
    event.preventDefault();
    if (session === undefined || request.trim() === "") return;
    run("Asking…", async () => ({ kind: "reply", reply: await client.ask(session, request) }));
  };

  const insert = (command: string) => {
    const done = onInsert(command);
    setInserted(done ? "yes" : "no");
    if (done) dialogRef.current?.close();
  };

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      onClose={onClose}
      className="top-[8%] m-0 mx-auto flex max-h-[84vh] w-[min(720px,94vw)] flex-col rounded-lg border border-current/15 bg-background p-4 text-sm text-foreground shadow-xl backdrop:bg-black/30"
    >
      <h2 id={titleId} className="mb-3 font-semibold">
        {mode === "settings"
          ? "AI assistant settings"
          : mode === "ask"
            ? "Ask AI for a command"
            : "Explain the output with AI"}
      </h2>
      <div className="min-h-0 flex-1 overflow-auto">
        {mode === "settings" ? (
          settings && (
            <SettingsForm
              client={client}
              settings={settings}
              onSaved={(saved) => {
                setSettings(saved);
                previewed.current = false;
                if (saved.enabled && initial !== "settings" && session !== undefined) {
                  setMode(initial);
                }
              }}
              onUnlock={onUnlock}
            />
          )
        ) : (
          <>
            {mode === "ask" && (
              <form onSubmit={ask} className="mb-3 flex gap-2">
                <input
                  autoFocus
                  aria-label="What should the command do?"
                  placeholder="What should the command do?"
                  value={request}
                  maxLength={2048}
                  onChange={(e) => setRequest(e.target.value)}
                  className={`${FIELD} min-w-0 flex-1`}
                />
                <button
                  type="submit"
                  className={PRIMARY}
                  disabled={step.kind === "busy" || request.trim() === ""}
                >
                  Ask
                </button>
              </form>
            )}
            <StepView
              step={step}
              server={settings?.baseUrl ?? ""}
              inserted={inserted}
              onSend={(preview) =>
                session !== undefined &&
                run("Asking…", async () => ({
                  kind: "reply",
                  reply: await client.explain(session, preview),
                }))
              }
              onInsert={insert}
              onCopy={onCopy}
            />
          </>
        )}
      </div>
      <div className="mt-3 flex items-center gap-2">
        {mode !== "settings" && (
          <button type="button" className={BUTTON} onClick={() => setMode("settings")}>
            Settings
          </button>
        )}
        <span className="flex-1" />
        <button type="button" className={BUTTON} onClick={() => dialogRef.current?.close()}>
          Close
        </button>
      </div>
    </dialog>
  );
}

/** Runs `work`, showing its answer, or asking for an unlock and trying again. */
function runStep(
  what: string,
  work: () => Promise<Step>,
  setStep: (step: Step) => void,
  onUnlock: (retry: () => void) => void,
): void {
  setStep({ kind: "busy", what });
  work()
    .then(setStep)
    .catch((error: unknown) => {
      if (error instanceof SessionCommandError && error.code === "vault_locked") {
        setStep({ kind: "idle" });
        onUnlock(() => runStep(what, work, setStep, onUnlock));
        return;
      }
      setStep({ kind: "failed", text: messageOf(error) });
    });
}

function StepView({
  step,
  server,
  inserted,
  onSend,
  onInsert,
  onCopy,
}: {
  step: Step;
  server: string;
  inserted: "yes" | "no" | undefined;
  onSend: (preview: number) => void;
  onInsert: (command: string) => void;
  onCopy: (text: string) => void;
}) {
  switch (step.kind) {
    case "busy":
      return <p className="opacity-70">{step.what}</p>;
    case "failed":
      return (
        <p role="alert" className={ALERT}>
          {step.text}
        </p>
      );
    case "preview":
      return (
        <div>
          <p className="mb-2">
            This goes to <strong>{server}</strong>, with secrets of a known shape hidden. Read it
            first:
          </p>
          <pre
            data-testid="ai-preview"
            className="max-h-72 overflow-auto rounded bg-current/5 p-2 font-mono text-xs whitespace-pre-wrap"
          >
            {step.text}
          </pre>
          <button type="button" className={`${PRIMARY} mt-3`} onClick={() => onSend(step.preview)}>
            Send
          </button>
        </div>
      );
    case "reply":
      return <Reply reply={step.reply} inserted={inserted} onInsert={onInsert} onCopy={onCopy} />;
  }
  return null;
}

function Reply({
  reply,
  inserted,
  onInsert,
  onCopy,
}: {
  reply: AiReplyView;
  inserted: "yes" | "no" | undefined;
  onInsert: (command: string) => void;
  onCopy: (text: string) => void;
}) {
  return (
    <div>
      <pre
        data-testid="ai-answer"
        className="max-h-64 overflow-auto rounded bg-current/5 p-2 text-xs whitespace-pre-wrap"
      >
        {reply.text}
      </pre>
      {reply.command !== null && (
        <div className="mt-3 rounded border border-current/15 p-2">
          <code data-testid="ai-command" className="block font-mono text-xs break-all">
            {reply.command}
          </code>
          {reply.findings.length > 0 && (
            <ul role="alert" className={`mt-2 list-disc pl-5 text-xs ${ALERT}`}>
              {reply.findings.map((finding) => (
                <li key={`${finding.rule}:${finding.command}`}>
                  The prod guard flags this: {finding.reason} ({finding.command})
                </li>
              ))}
            </ul>
          )}
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <button
              type="button"
              className={reply.findings.length > 0 ? BUTTON : PRIMARY}
              onClick={() => onInsert(reply.command ?? "")}
            >
              {reply.findings.length > 0 ? "Put on the line anyway" : "Put on the line"}
            </button>
            <button type="button" className={BUTTON} onClick={() => onCopy(reply.command ?? "")}>
              Copy
            </button>
          </div>
          {inserted === "no" && (
            <p role="alert" className={`mt-2 text-xs ${ALERT}`}>
              The pane is not waiting at an empty prompt: copy the command instead.
            </p>
          )}
          <p className="mt-2 text-xs opacity-70">
            Nothing runs by itself. Read the command, then press Enter in the terminal.
          </p>
        </div>
      )}
    </div>
  );
}

function SettingsForm({
  client,
  settings,
  onSaved,
  onUnlock,
}: {
  client: AiClient;
  settings: AiSettingsView;
  onSaved: (settings: AiSettingsView) => void;
  onUnlock: (retry: () => void) => void;
}) {
  const [enabled, setEnabled] = useState(settings.enabled);
  const [baseUrl, setBaseUrl] = useState(settings.baseUrl ?? "");
  const [model, setModel] = useState(settings.model ?? "");
  const [localOutput, setLocalOutput] = useState(settings.localOutput);
  const [key, setKey] = useState("");
  const [status, setStatus] = useState<{ error: boolean; text: string } | undefined>();
  // A key goes with the saved address only: save a new one first.
  const unsaved =
    baseUrl.trim().replace(/\/+$/u, "") !== (settings.baseUrl ?? "") ||
    model.trim() !== (settings.model ?? "");

  const done = (text: string) => (saved: AiSettingsView) => {
    setStatus({ error: false, text });
    // As the backend wrote it (scheme and host lowercase, no trailing /).
    setBaseUrl(saved.baseUrl ?? "");
    setModel(saved.model ?? "");
    onSaved(saved);
  };
  const failed = (retry: () => void) => (error: unknown) => {
    if (error instanceof SessionCommandError && error.code === "vault_locked") {
      onUnlock(retry);
      return;
    }
    setStatus({ error: true, text: messageOf(error) });
  };

  const save = (event: FormEvent) => {
    event.preventDefault();
    client
      .saveSettings({ enabled, baseUrl, model, localOutput })
      .then(done("Saved."))
      .catch(failed(() => {}));
  };
  const saveKey = () => {
    const attempt = () => {
      client
        .saveKey(key, baseUrl)
        .then((saved) => {
          setKey("");
          done("The key is saved in the vault.")(saved);
        })
        .catch(failed(attempt));
    };
    attempt();
  };
  const forgetKey = () => {
    const attempt = () => {
      client.clearKey().then(done("The key is gone.")).catch(failed(attempt));
    };
    attempt();
  };

  return (
    <form onSubmit={save} className="flex flex-col gap-3">
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        Turn on the AI assistant
      </label>
      <label className="flex flex-col gap-1">
        Server (OpenAI-compatible)
        <input
          value={baseUrl}
          placeholder="https://api.openai.com/v1 or http://localhost:11434/v1"
          onChange={(e) => setBaseUrl(e.target.value)}
          className={FIELD}
        />
      </label>
      <label className="flex flex-col gap-1">
        Model
        <input
          value={model}
          placeholder="gpt-4o-mini, llama3.1, …"
          onChange={(e) => setModel(e.target.value)}
          className={FIELD}
        />
      </label>
      <label className="flex items-center gap-2">
        <input
          type="checkbox"
          checked={localOutput}
          onChange={(e) => setLocalOutput(e.target.checked)}
        />
        Local terminals may send their output
      </label>
      <p className="text-xs opacity-70">
        A saved host&apos;s output goes only where its settings let the AI read it. A server on
        localhost may still pass what it gets to another machine.
      </p>
      <div>
        <button type="submit" className={PRIMARY}>
          Save
        </button>
      </div>
      <fieldset className="flex flex-col gap-2 border-t border-current/15 pt-3">
        <legend className="sr-only">API key</legend>
        <p>
          {settings.hasKey
            ? "A key is saved in the vault for this server."
            : "No key saved. A local server may need none."}
        </p>
        {unsaved && settings.baseUrl !== null && (
          <p className="text-xs opacity-70">Save the server first: a key goes with it.</p>
        )}
        <div className="flex gap-2">
          <input
            type="password"
            aria-label="API key"
            placeholder="API key"
            autoComplete="off"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            className={`${FIELD} min-w-0 flex-1`}
          />
          <button
            type="button"
            className={BUTTON}
            disabled={key === "" || settings.baseUrl === null || unsaved}
            onClick={saveKey}
          >
            Save key
          </button>
          {settings.hasKey && (
            <button type="button" className={BUTTON} onClick={forgetKey}>
              Forget key
            </button>
          )}
        </div>
      </fieldset>
      {status && (
        <p role={status.error ? "alert" : "status"} className={status.error ? ALERT : "opacity-80"}>
          {status.text}
        </p>
      )}
    </form>
  );
}
