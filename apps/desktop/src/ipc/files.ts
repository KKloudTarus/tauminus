// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import { Channel } from "@tauri-apps/api/core";

import {
  type AnswerSpec,
  type ChangeSpec,
  type DroppedFiles,
  type EditView,
  type EntryView,
  type Listing,
  type PromptRequest,
  type SftpHandle,
  type TransferView,
  commands,
} from "./bindings.ts";
import { type PromptHandler, SessionCommandError } from "./sessions.ts";

export type { ChangeSpec, DroppedFiles, EditView, EntryView, Listing, SftpHandle, TransferView };

/** Files on saved hosts and on this machine, and the transfers between them. */
export interface FilesClient {
  open(host: string, onPrompt: PromptHandler): Promise<SftpHandle>;
  close(id: number): Promise<void>;
  list(id: number, path: string): Promise<Listing>;
  change(id: number, change: ChangeSpec): Promise<void>;
  localList(path: string): Promise<Listing>;
  localHome(): Promise<string>;
  download(id: number, remote: string, into: string, overwrite: boolean): Promise<number>;
  upload(id: number, local: string, into: string, overwrite: boolean): Promise<number>;
  transfers(): Promise<TransferView[]>;
  cancel(transfer: number): Promise<boolean>;
  /** Opens a remote file in a local text editor; each save goes back up. */
  edit(id: number, remote: string): Promise<number>;
  /** Ends an edit after its last save went up. */
  editDone(edit: number): Promise<boolean>;
  edits(): Promise<EditView[]>;
  /** Calls `onDrop` with each later drop of files on the window. */
  onDrop(onDrop: (files: DroppedFiles) => void): Promise<void>;
}

const CANCEL: AnswerSpec = { kind: "cancel" };

type Result<T> = Promise<
  | { status: "ok"; data: T }
  | { status: "error"; error: { code: SessionCommandError["code"]; message: string } }
>;

async function unwrap<T>(result: Result<T>): Promise<T> {
  const outcome = await result;
  if (outcome.status === "error") throw new SessionCommandError(outcome.error);
  return outcome.data;
}

export const tauriFiles: FilesClient = {
  open: (host, onPrompt) => {
    const prompts = new Channel<PromptRequest>();
    prompts.onmessage = ({ id, prompt }) => {
      onPrompt(prompt)
        .catch(() => CANCEL)
        .then((answer) => commands.sshPromptReply(id, answer))
        .catch((error: unknown) => console.warn("prompt reply failed", error));
    };
    return unwrap(commands.sftpOpen(host, prompts));
  },
  close: (id) => commands.sftpClose(id).then(() => {}),
  list: (id, path) => unwrap(commands.sftpList(id, path)),
  change: (id, change) => unwrap(commands.sftpChange(id, change)).then(() => {}),
  localList: (path) => unwrap(commands.localList(path)),
  localHome: () => unwrap(commands.localHome()),
  download: (id, remote, into, overwrite) =>
    unwrap(commands.transferDownload(id, remote, into, overwrite)),
  upload: (id, local, into, overwrite) =>
    unwrap(commands.transferUpload(id, local, into, overwrite)),
  transfers: () => commands.transfersStatus(),
  cancel: (transfer) => commands.transferCancel(transfer),
  edit: (id, remote) => unwrap(commands.sftpEdit(id, remote)),
  editDone: (edit) => commands.editDone(edit),
  edits: () => commands.editsStatus(),
  onDrop: (onDrop) => {
    const drops = new Channel<DroppedFiles>();
    drops.onmessage = onDrop;
    return commands.fileDrops(drops).then(() => {});
  },
};
