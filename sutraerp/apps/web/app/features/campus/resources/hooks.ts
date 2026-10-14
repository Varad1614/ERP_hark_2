/**
 * React glue of the slice-8D screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix so `useCommand` refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpResourcesApi, type ResourcesApi } from "./api";
import { createMockResourcesApi } from "./mock";

let fallback: ResourcesApi | null = null;
const Ctx = createContext<ResourcesApi | null>(null);

export function ResourcesApiProvider({ api, children }: { api: ResourcesApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useResourcesApi(): ResourcesApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpResourcesApi() : createMockResourcesApi();
  return fallback;
}

export const rkeys = {
  scoped: (owner: string, what: string) => ["institution", "8d", owner, what] as const,
};
