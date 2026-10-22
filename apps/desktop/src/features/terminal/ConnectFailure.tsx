// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import type { ErrorCode } from "@/ipc/index.ts";

interface ConnectFailureProps {
  target: string;
  code: ErrorCode | undefined;
  message: string;
  onRetry: () => void;
  /** Opens the vault, then retries; for a host whose identity is in it. */
  onUnlockVault?: (retry: () => void) => void;
}

/** Why an SSH connect failed, and what to do next. */
export function ConnectFailure({
  target,
  code,
  message,
  onRetry,
  onUnlockVault,
}: ConnectFailureProps) {
  const danger = code === "host_key_changed" || code === "host_key_revoked";
  return (
    <div className="absolute inset-0 flex items-center justify-center bg-background p-4">
      <div
        role="alert"
        className={`w-[min(560px,100%)] rounded-lg border p-4 text-sm ${
          danger ? "border-red-500/60 bg-red-500/10" : "border-current/15"
        }`}
      >
        <h2 className="mb-2 font-semibold">{heading(code, target)}</h2>
        <p className="mb-2 opacity-80">{advice(code)}</p>
        <p className="mb-4 font-mono text-xs break-words opacity-70">{message}</p>
        {code === "vault_locked" && onUnlockVault && (
          <button
            type="button"
            onClick={() => onUnlockVault(onRetry)}
            className="mr-2 rounded bg-foreground px-3 py-1.5 text-background hover:opacity-90"
          >
            Unlock the vault
          </button>
        )}
        {!danger && (
          <button
            type="button"
            onClick={onRetry}
            className="rounded border border-current/20 px-3 py-1.5 hover:bg-current/10"
          >
            Try again
          </button>
        )}
      </div>
    </div>
  );
}

function heading(code: ErrorCode | undefined, target: string): string {
  switch (code) {
    case "host_key_changed":
      return `Warning: the host key for ${target} has changed`;
    case "host_key_revoked":
      return `The host key for ${target} is revoked`;
    case "auth_failed":
      return `Could not sign in to ${target}`;
    case "cancelled":
      return `Connection to ${target} cancelled`;
    case "connect_failed":
      return `Could not reach ${target}`;
    case "vault_locked":
      return `Unlock the vault to sign in to ${target}`;
    default:
      return `Could not connect to ${target}`;
  }
}

function advice(code: ErrorCode | undefined): string {
  switch (code) {
    case "host_key_changed":
      return (
        "Someone may be intercepting this connection, or the host was reinstalled. Tauminus will not " +
        "connect. Confirm the new fingerprint with the host's administrator, then remove the old " +
        "entry from the known_hosts file named below."
      );
    case "host_key_revoked":
      return "The key this host presented is marked @revoked in known_hosts. Tauminus will not connect.";
    case "auth_failed":
      return "Check the user name, and that your key is loaded in ssh-agent or listed for this host.";
    case "cancelled":
      return "Nothing was sent to the server.";
    case "connect_failed":
      return "Check the host name and port, your network, and any proxy or jump host.";
    case "vault_locked":
      return "This host signs in with an identity kept in the vault.";
    default:
      return "The server or a proxy ended the connection unexpectedly.";
  }
}
