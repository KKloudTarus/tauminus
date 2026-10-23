// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// The assistant for the `e2e` build: settings in memory, canned answers.
// Which output may go is the backend's decision, tested in Rust; here the
// local-output setting stands for it.

import type { AiClient, AiReplyView, AiSettingsView } from "./ai.ts";
import { sharedFakeVault } from "./fake-vault.ts";
import { SessionCommandError } from "./sessions.ts";

const fail = (code: SessionCommandError["code"], message: string) =>
  new SessionCommandError({ code, message });

export function fakeAi(): AiClient {
  let settings: AiSettingsView = {
    enabled: false,
    baseUrl: null,
    model: null,
    localOutput: false,
    hasKey: false,
    loopback: false,
  };
  let next = 1;
  const previews = new Map<number, number>();
  const ready = () => {
    if (!settings.enabled) throw fail("not_allowed", "the AI assistant is off: turn it on");
  };
  return {
    settings: () => Promise.resolve(settings),
    saveSettings: (spec) => {
      // Scheme and host lowercase, as the backend writes them.
      const url = spec.baseUrl
        .trim()
        .replace(/^[a-z]+:\/\/[^/]+/iu, (origin) => origin.toLowerCase());
      if (url !== "" && !/^https:\/\/|^http:\/\/(localhost|127\.0\.0\.1)(:|\/|$)/u.test(url)) {
        return Promise.reject(fail("invalid_argument", "http:// is only for this computer"));
      }
      if (spec.enabled && url === "") {
        return Promise.reject(fail("invalid_argument", "set the AI server's address first"));
      }
      const sameServer = url === settings.baseUrl;
      settings = {
        enabled: spec.enabled,
        baseUrl: url === "" ? null : url.replace(/\/+$/u, ""),
        model: url === "" ? null : spec.model.trim(),
        localOutput: spec.localOutput,
        hasKey: settings.hasKey && sameServer,
        loopback: url.startsWith("http://"),
      };
      return Promise.resolve(settings);
    },
    saveKey: (_key, baseUrl) => {
      if (baseUrl.replace(/\/+$/u, "") !== settings.baseUrl) {
        return Promise.reject(fail("invalid_argument", "save the address first"));
      }
      if (!sharedFakeVault().unlocked()) return Promise.reject(fail("vault_locked", "locked"));
      settings = { ...settings, hasKey: true };
      return Promise.resolve(settings);
    },
    clearKey: () => {
      settings = { ...settings, hasKey: false };
      return Promise.resolve(settings);
    },
    ask: (_session, request) => {
      ready();
      return Promise.resolve(answer(request));
    },
    preview: (session) => {
      ready();
      if (!settings.localOutput) {
        throw fail("not_allowed", "this session does not let the AI read its output");
      }
      const preview = next++;
      previews.set(session, preview);
      return Promise.resolve({ preview, text: "make: *** [all] Error 2\nexport KEY=•••" });
    },
    explain: (session, preview) => {
      ready();
      if (previews.get(session) !== preview) throw fail("cancelled", "that preview is gone");
      previews.delete(session);
      return Promise.resolve({
        text: "The build failed.\n```sh\nmake clean\n```",
        command: "make clean",
        findings: [],
      });
    },
  };
}

function answer(request: string): AiReplyView {
  if (/delete|remove/iu.test(request)) {
    return {
      text: "```sh\nrm -rf /srv/data\n```\nDeletes it all.",
      command: "rm -rf /srv/data",
      findings: [
        {
          rule: "recursive-delete",
          reason: "deletes files recursively",
          command: "rm -rf /srv/data",
        },
      ],
    };
  }
  return { text: "```sh\nls -la\n```\nLists the files.", command: "ls -la", findings: [] };
}
