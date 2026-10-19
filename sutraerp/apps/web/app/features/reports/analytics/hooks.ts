/**
 * React glue of the slice-8C reports screens: API instance (context, mock or HTTP) and query keys
 * under the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them
 * after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpAnalyticsApi, type AnalyticsApi } from "./api";
import { createMockAnalyticsApi } from "./mock";

let fallback: AnalyticsApi | null = null;
const Ctx = createContext<AnalyticsApi | null>(null);

export function AnalyticsApiProvider({ api, children }: { api: AnalyticsApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useAnalyticsApi(): AnalyticsApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpAnalyticsApi() : createMockAnalyticsApi();
  return fallback;
}

export const akeys = (owner: string, what: string, extra = "") => ["institution", "8c-reports", owner, what, extra] as const;
