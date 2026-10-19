/**
 * React glue of the slice-8A screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpFacilitiesApi, type FacilitiesApi } from "./api";
import { createMockFacilitiesApi } from "./mock";

let fallback: FacilitiesApi | null = null;
const Ctx = createContext<FacilitiesApi | null>(null);

export function FacilitiesApiProvider({ api, children }: { api: FacilitiesApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useFacilitiesApi(): FacilitiesApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpFacilitiesApi() : createMockFacilitiesApi();
  return fallback;
}

export const fkeys = (owner: string, what: string, extra = "") => ["institution", "8a", owner, what, extra] as const;
