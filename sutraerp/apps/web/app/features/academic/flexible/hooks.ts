/**
 * React glue of the slice-4C screens: API instance (context, mock or HTTP) and query keys.
 * Keys live under the `institution` prefix so `useCommand` (features/institution/hooks)
 * refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpFlexibleApi, type FlexibleApi } from "./api";
import { createMockFlexibleApi } from "./mock";

let fallback: FlexibleApi | null = null;
const Ctx = createContext<FlexibleApi | null>(null);

export function FlexibleApiProvider({ api, children }: { api: FlexibleApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useFlexibleApi(): FlexibleApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpFlexibleApi() : createMockFlexibleApi();
  return fallback;
}

export const fkeys = {
  scoped: (owner: string, what: string) => ["institution", "4c", owner, what] as const,
};
