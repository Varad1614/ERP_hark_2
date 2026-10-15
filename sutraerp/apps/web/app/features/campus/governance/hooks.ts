/**
 * React glue of the slice-9E governance screen: API instance (context, mock or HTTP) and query
 * keys under the `institution` prefix, so `useCommand` refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpGovernanceApi, type GovernanceApi } from "./api";
import { createMockGovernanceApi } from "./mock";

let fallback: GovernanceApi | null = null;
const Ctx = createContext<GovernanceApi | null>(null);

export function GovernanceApiProvider({ api, children }: { api: GovernanceApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useGovernanceApi(): GovernanceApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpGovernanceApi() : createMockGovernanceApi();
  return fallback;
}

export const gkeys = (owner: string, what: string, extra = "") => ["institution", "9e-governance", owner, what, extra] as const;
