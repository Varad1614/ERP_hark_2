/**
 * React glue of the slice-5A screens: API instance (context, mock or HTTP) and query keys.
 * Keys live under the `institution` prefix so `useCommand` (features/institution/hooks)
 * refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpChargesApi, type ChargesApi } from "./api";
import { createMockChargesApi } from "./mock";

let fallback: ChargesApi | null = null;
const Ctx = createContext<ChargesApi | null>(null);

export function ChargesApiProvider({ api, children }: { api: ChargesApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useChargesApi(): ChargesApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpChargesApi() : createMockChargesApi();
  return fallback;
}

export const fkeys = {
  scoped: (owner: string, what: string) => ["institution", "5a", owner, what] as const,
};

/** Statuses after which a charge is no longer a live receivable (no lifecycle buttons). */
export const FINAL_CHARGE = ["cancelled", "waived", "written_off", "archived"];
