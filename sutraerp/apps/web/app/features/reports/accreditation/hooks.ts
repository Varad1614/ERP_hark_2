/**
 * React glue of the slice-9E accreditation screen: API instance (context, mock or HTTP) and query
 * keys under the `institution` prefix, so `useCommand` refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpAccreditationApi, type AccreditationApi } from "./api";
import { createMockAccreditationApi } from "./mock";

let fallback: AccreditationApi | null = null;
const Ctx = createContext<AccreditationApi | null>(null);

export function AccreditationApiProvider({ api, children }: { api: AccreditationApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useAccreditationApi(): AccreditationApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpAccreditationApi() : createMockAccreditationApi();
  return fallback;
}

export const ackeys = (owner: string, what: string, extra = "") => ["institution", "9e-accreditation", owner, what, extra] as const;
