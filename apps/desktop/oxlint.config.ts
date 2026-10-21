// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

import reactHooks from "eslint-plugin-react-hooks";
import { defineConfig } from "oxlint";

// The React Compiler rules ship only in the JS plugin. oxlint's built-in
// react-hooks port is not 1:1, so load the JS plugin under its own prefix.
const reactCompilerRules = Object.fromEntries(
  Object.entries(reactHooks.configs["recommended-latest"].rules ?? {}).map(([rule, level]) => [
    rule.replace("react-hooks/", "react-hooks-js/"),
    level,
  ]),
);

export default defineConfig({
  plugins: ["react", "typescript", "oxc", "import", "jsx-a11y", "unicorn", "vitest"],
  jsPlugins: [{ name: "react-hooks-js", specifier: "eslint-plugin-react-hooks" }],
  categories: {
    correctness: "error",
    suspicious: "error",
    perf: "warn",
  },
  options: {
    typeAware: true,
    denyWarnings: true,
    reportUnusedDisableDirectives: "error",
  },
  ignorePatterns: ["dist/**", "src-tauri/**", "src/ipc/bindings.ts"],
  rules: {
    ...reactCompilerRules,
    "import/no-cycle": "error",
    "import/no-unassigned-import": ["error", { allow: ["**/*.css"] }],
    // React 17+ JSX transform: no React import needed.
    "react/react-in-jsx-scope": "off",
    "no-console": ["error", { allow: ["error", "warn"] }],
    "typescript/no-floating-promises": "error",
    "typescript/no-misused-promises": "error",
    "typescript/consistent-type-imports": "error",
    "no-restricted-imports": [
      "error",
      {
        patterns: [
          {
            group: ["@tauri-apps/**"],
            message: "Call the backend through @/ipc so every IPC call stays typed and testable.",
          },
          {
            group: ["@/ipc/bindings*", "**/ipc/bindings*"],
            message: "Import from @/ipc; bindings.ts is generated and wrapped there.",
          },
        ],
      },
    ],
  },
  overrides: [
    {
      files: ["src/ipc/**"],
      rules: {
        "no-restricted-imports": "off",
        // Tauri's Channel exposes only `onmessage`, no addEventListener.
        "unicorn/prefer-add-event-listener": "off",
      },
    },
  ],
});
