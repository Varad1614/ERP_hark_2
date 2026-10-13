/**
 * React glue of the slice-5C screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpLedgerApi, type LedgerApi } from "./api";
import { createMockLedgerApi } from "./mock";

let fallback: LedgerApi | null = null;
const Ctx = createContext<LedgerApi | null>(null);

export function LedgerApiProvider({ api, children }: { api: LedgerApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useLedgerApi(): LedgerApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpLedgerApi() : createMockLedgerApi();
  return fallback;
}

export const lkeys = {
  scoped: (owner: string, what: string) => ["institution", "5c", owner, what] as const,
};
