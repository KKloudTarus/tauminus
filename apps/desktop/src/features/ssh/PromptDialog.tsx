// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import {
  type FormEvent,
  type ReactNode,
  type RefObject,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";

import type { AnswerSpec, PromptKind } from "@/ipc/index.ts";

import { serverText } from "./server-text.ts";

interface PromptDialogProps {
  prompt: PromptKind;
  onAnswer: (answer: AnswerSpec) => void;
}

const CANCEL: AnswerSpec = { kind: "cancel" };

/**
 * A question from an SSH connect, shown over its pane. Not a page-wide modal:
 * other panes stay usable while one connect waits. Escape cancels.
 */
export function PromptDialog({ prompt, onAnswer }: PromptDialogProps) {
  const titleId = useId();
  return (
    <div className="absolute inset-0 z-10 flex items-center justify-center bg-terminal/80 p-4">
      <dialog
        open
        aria-labelledby={titleId}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onAnswer(CANCEL);
          }
        }}
        className="panel static m-0 w-[min(520px,100%)] p-4"
      >
        <PromptBody prompt={prompt} titleId={titleId} onAnswer={onAnswer} />
      </dialog>
    </div>
  );
}

/** Moves focus to `ref` once, when the prompt appears. */
function useInitialFocus(ref: RefObject<HTMLElement | null>) {
  useEffect(() => {
    ref.current?.focus();
  }, [ref]);
}

function PromptBody({ prompt, titleId, onAnswer }: PromptDialogProps & { titleId: string }) {
  switch (prompt.kind) {
    case "unknownHostKey":
      return <HostKeyPrompt prompt={prompt} titleId={titleId} onAnswer={onAnswer} />;
    case "password":
      return (
        <SecretsForm
          key={`password-${prompt.endpoint}`}
          title={`Password for ${prompt.endpoint}`}
          titleId={titleId}
          fields={[{ text: "Password", echo: false }]}
          onAnswer={(values) => onAnswer({ kind: "secret", value: values[0] ?? "" })}
          onCancel={() => onAnswer(CANCEL)}
        />
      );
    case "passphrase":
      return (
        <SecretsForm
          key={`passphrase-${prompt.keyPath}`}
          title="Passphrase for key"
          titleId={titleId}
          detail={<code className="break-all">{prompt.keyPath}</code>}
          fields={[{ text: "Passphrase", echo: false }]}
          onAnswer={(values) => onAnswer({ kind: "secret", value: values[0] ?? "" })}
          onCancel={() => onAnswer(CANCEL)}
        />
      );
    case "keyboardInteractive": {
      const name = serverText(prompt.name);
      const instructions = serverText(prompt.instructions);
      return (
        <SecretsForm
          title={name || `Sign in to ${prompt.endpoint}`}
          titleId={titleId}
          detail={
            <>
              <span className="block text-xs opacity-60">
                Asked by the server {prompt.endpoint}
              </span>
              {instructions && (
                <span className="mt-1 block whitespace-pre-wrap">{instructions}</span>
              )}
            </>
          }
          fields={prompt.fields.map((field) => ({
            text: serverText(field.text) || "Response",
            echo: field.echo,
          }))}
          onAnswer={(values) => onAnswer({ kind: "responses", values })}
          onCancel={() => onAnswer(CANCEL)}
        />
      );
    }
    default:
      return null;
  }
}

function HostKeyPrompt({
  prompt,
  titleId,
  onAnswer,
}: {
  prompt: Extract<PromptKind, { kind: "unknownHostKey" }>;
  titleId: string;
  onAnswer: (answer: AnswerSpec) => void;
}) {
  const host = prompt.port === 22 ? prompt.host : `[${prompt.host}]:${prompt.port}`;
  // Cancel takes focus, so a stray Enter never trusts a key.
  const cancelRef = useRef<HTMLButtonElement>(null);
  useInitialFocus(cancelRef);
  return (
    <>
      <h2 id={titleId} className="mb-2 font-semibold">
        New host: {host}
      </h2>
      <p className="mb-2 opacity-80">
        Tauminus has not connected to this host before. Check that the fingerprint matches the one
        your administrator or provider published before you trust it.
      </p>
      {prompt.certificateExpected && (
        <p
          role="alert"
          className="mb-3 rounded-lg border border-danger/50 bg-danger/10 p-2.5 text-danger"
        >
          A certificate authority is configured for this host, but the host did not present a
          certificate it signed. Someone may be intercepting the connection. Cancel unless you know
          why.
        </p>
      )}
      <dl className="mb-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-lg border border-line bg-terminal p-3 text-[13px]">
        <dt className="text-faint">Key type</dt>
        <dd>{prompt.algorithm}</dd>
        <dt className="text-faint">Fingerprint</dt>
        <dd>
          <code
            className="font-mono text-xs leading-5 break-all select-all"
            data-testid="host-key-fingerprint"
          >
            {prompt.fingerprint}
          </code>
        </dd>
      </dl>
      <div className="flex flex-wrap justify-end gap-2">
        <Button buttonRef={cancelRef} onClick={() => onAnswer(CANCEL)}>
          Cancel
        </Button>
        <Button onClick={() => onAnswer({ kind: "trustHostKey", remember: false })}>
          Trust once
        </Button>
        <Button primary onClick={() => onAnswer({ kind: "trustHostKey", remember: true })}>
          Trust and remember
        </Button>
      </div>
    </>
  );
}

interface Field {
  text: string;
  echo: boolean;
}

function SecretsForm({
  title,
  titleId,
  detail,
  fields,
  onAnswer,
  onCancel,
}: {
  title: string;
  titleId: string;
  detail?: ReactNode;
  fields: readonly Field[];
  onAnswer: (values: string[]) => void;
  onCancel: () => void;
}) {
  const [values, setValues] = useState(() => fields.map(() => ""));
  const baseId = useId();
  const firstRef = useRef<HTMLInputElement>(null);
  useInitialFocus(firstRef);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const answer = values;
    // Drop the secrets from component state as soon as they are sent.
    setValues(fields.map(() => ""));
    onAnswer(answer);
  };
  return (
    <form onSubmit={submit}>
      <h2 id={titleId} className="mb-2 font-semibold">
        {title}
      </h2>
      {detail && <div className="mb-3 opacity-80">{detail}</div>}
      {fields.map((field, index) => (
        // oxlint-disable-next-line react/no-array-index-key -- one prompt's fields never reorder
        <label key={`${baseId}-${index}`} className="mb-3 block">
          <span className="mb-1 block whitespace-pre-wrap opacity-80">{field.text}</span>
          <input
            ref={index === 0 ? firstRef : undefined}
            type={field.echo ? "text" : "password"}
            autoComplete="off"
            spellCheck={false}
            value={values[index] ?? ""}
            onChange={(event) => {
              const next = [...values];
              next[index] = event.target.value;
              setValues(next);
            }}
            className="field"
          />
        </label>
      ))}
      <div className="flex justify-end gap-2">
        <Button onClick={onCancel}>Cancel</Button>
        <Button primary type="submit">
          Continue
        </Button>
      </div>
    </form>
  );
}

function Button({
  primary = false,
  type = "button",
  buttonRef,
  onClick,
  children,
}: {
  primary?: boolean;
  type?: "button" | "submit";
  buttonRef?: RefObject<HTMLButtonElement | null>;
  onClick?: () => void;
  children: ReactNode;
}) {
  return (
    <button
      ref={buttonRef}
      type={type}
      onClick={onClick}
      className={primary ? "btn btn-primary" : "btn"}
    >
      {children}
    </button>
  );
}
