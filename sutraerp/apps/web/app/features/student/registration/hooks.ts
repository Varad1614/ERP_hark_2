/**
 * React glue of the slice-3B screens: API instance (context, mock or HTTP) and query keys.
 * Keys live under the `institution` prefix so `useCommand` (features/institution/hooks)
 * refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpRegistrationApi, type RegistrationApi } from "./api";
import { createMockRegistrationApi } from "./mock";

let fallback: RegistrationApi | null = null;
const Ctx = createContext<RegistrationApi | null>(null);

export function RegistrationApiProvider({ api, children }: { api: RegistrationApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useRegistrationApi(): RegistrationApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpRegistrationApi() : createMockRegistrationApi();
  return fallback;
}

export const rkeys = {
  scoped: (owner: string, what: string) => ["institution", "3b", owner, what] as const,
};
