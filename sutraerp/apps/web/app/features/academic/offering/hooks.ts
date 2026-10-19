/**
 * React glue of the slice-4A screens: API instance (context, mock or HTTP) and query keys.
 * Keys live under the `institution` prefix so `useCommand` (features/institution/hooks)
 * refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpOfferingApi, type OfferingApi } from "./api";
import { createMockOfferingApi } from "./mock";

let fallback: OfferingApi | null = null;
const Ctx = createContext<OfferingApi | null>(null);

export function OfferingApiProvider({ api, children }: { api: OfferingApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useOfferingApi(): OfferingApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpOfferingApi() : createMockOfferingApi();
  return fallback;
}

export const okeys = {
  scoped: (owner: string, what: string) => ["institution", "4a", owner, what] as const,
};
