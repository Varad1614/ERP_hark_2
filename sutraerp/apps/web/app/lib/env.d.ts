/// <reference types="vite/client" />
interface ImportMetaEnv {
  readonly VITE_AUTH_MODE?: "mock" | "api" | "oidc";
  readonly VITE_API_BASE?: string;
  readonly VITE_DEV_PASSWORD?: string;
  /** "1" serves the dev-only /dev/catalogue route in a production build (e2e builds). */
  readonly VITE_UI_CATALOGUE?: string;
}
interface ImportMeta {
  readonly env: ImportMetaEnv;
}
