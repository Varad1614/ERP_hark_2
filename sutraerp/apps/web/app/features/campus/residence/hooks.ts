/**
 * React glue of the slice-8B screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpResidenceApi, type ResidenceApi } from "./api";
import { createMockResidenceApi } from "./mock";

let fallback: ResidenceApi | null = null;
const Ctx = createContext<ResidenceApi | null>(null);

export function ResidenceApiProvider({ api, children }: { api: ResidenceApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useResidenceApi(): ResidenceApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpResidenceApi() : createMockResidenceApi();
  return fallback;
}

export const rkeys = (owner: string, what: string) => ["institution", "8b", owner, what] as const;
