/**
 * React glue of the slice-7C screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix so `useCommand` refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpPayrollApi, type PayrollApi } from "./api";
import { createMockPayrollApi } from "./mock";

let fallback: PayrollApi | null = null;
const Ctx = createContext<PayrollApi | null>(null);

export function PayrollApiProvider({ api, children }: { api: PayrollApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function usePayrollApi(): PayrollApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpPayrollApi() : createMockPayrollApi();
  return fallback;
}

export const pkeys = {
  scoped: (owner: string, what: string) => ["institution", "7c", owner, what] as const,
};

/** Next run commands offered for a run status (the API enforces the lifecycle). */
export function runActions(status: string): Array<"calculate" | "validate" | "submit" | "approve" | "reject" | "finalise" | "post" | "issue" | "close" | "reprocess"> {
  switch (status) {
    case "created":
    case "reprocessing":
      return ["calculate"];
    case "calculated":
      return ["calculate", "validate"];
    case "validating":
      return ["submit"];
    case "pending_approval":
      return ["approve", "reject"];
    case "approved":
      return ["finalise"];
    case "finalized":
      return ["post", "issue"];
    case "posted":
      return ["issue", "close"];
    case "failed":
      return ["reprocess"];
    default:
      return [];
  }
}
