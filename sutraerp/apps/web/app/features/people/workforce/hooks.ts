/**
 * React glue of the slice-7A screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix so `useCommand` refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpWorkforceApi, type WorkforceApi } from "./api";
import { createMockWorkforceApi } from "./mock";

let fallback: WorkforceApi | null = null;
const Ctx = createContext<WorkforceApi | null>(null);

export function WorkforceApiProvider({ api, children }: { api: WorkforceApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useWorkforceApi(): WorkforceApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpWorkforceApi() : createMockWorkforceApi();
  return fallback;
}

export const wkeys = {
  scoped: (owner: string, what: string) => ["institution", "7a", owner, what] as const,
};
