// @ts-check
import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["**/dist/**", "**/build/**", "**/node_modules/**", "**/.react-router/**", "target/**", "packages/api-client/src/schema.d.ts", ".cache/**"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: { globals: { ...globals.browser }, ecmaVersion: 2023 },
    plugins: { "react-hooks": reactHooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
      "@typescript-eslint/consistent-type-imports": "error",
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
    },
  },
  {
    // Playwright end-to-end scripts run in Node and evaluate snippets in the browser.
    files: ["apps/web/e2e/**/*.mjs"],
    languageOptions: { globals: { ...globals.node, ...globals.browser }, ecmaVersion: 2023, sourceType: "module" },
  },
  {
    // Image build helpers (deploy/docker/csp-hashes.mjs) run in Node; other deploy scripts declare /* global */ themselves.
    files: ["deploy/docker/**/*.mjs"],
    languageOptions: { globals: { ...globals.node }, ecmaVersion: 2023, sourceType: "module" },
  },
  {
    // Golden-vector generator for logic ported from HeyPuter/everything (tools/golden/README.md).
    files: ["tools/**/*.mjs"],
    languageOptions: { globals: { ...globals.node }, ecmaVersion: 2023, sourceType: "module" },
  },
);
