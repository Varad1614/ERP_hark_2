/**
 * React glue of the slice-5B screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpPaymentsApi, type PaymentsApi } from "./api";
import { createMockPaymentsApi } from "./mock";

let fallback: PaymentsApi | null = null;
const Ctx = createContext<PaymentsApi | null>(null);

export function PaymentsApiProvider({ api, children }: { api: PaymentsApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function usePaymentsApi(): PaymentsApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpPaymentsApi() : createMockPaymentsApi();
  return fallback;
}

export const pkeys = {
  scoped: (owner: string, what: string) => ["institution", "5b", owner, what] as const,
};
