/// <reference types="vite/client" />
interface ImportMetaEnv {
  /** Unset in a build = `oidc`; `react-router dev` / Vitest fall back to `mock` (lib/api.ts). */
  readonly VITE_AUTH_MODE?: "mock" | "api" | "oidc" | "";
  readonly VITE_API_BASE?: string;
  /** "true" = dev persona picker + account-menu role switcher (dev / CI builds only; lib/dev-personas.ts). */
  readonly VITE_DEV_PERSONAS?: string;
  /** "1" serves the dev-only /dev/catalogue route in a production build (e2e builds). */
  readonly VITE_UI_CATALOGUE?: string;
}
interface ImportMeta {
  readonly env: ImportMetaEnv;
}
