/**
 * React glue of the slice-7B screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpLeaveApi, type LeaveApi } from "./api";
import { createMockLeaveApi } from "./mock";

let fallback: LeaveApi | null = null;
const Ctx = createContext<LeaveApi | null>(null);

export function LeaveApiProvider({ api, children }: { api: LeaveApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useLeaveApi(): LeaveApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpLeaveApi() : createMockLeaveApi();
  return fallback;
}

export const lkeys = (owner: string, what: string) => ["institution", "7b", owner, what] as const;
