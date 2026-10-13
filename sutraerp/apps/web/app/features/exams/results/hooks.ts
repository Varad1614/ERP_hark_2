/**
 * React glue of the slice-6C screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix so `useCommand` refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpResultsApi, type ResultsApi } from "./api";
import { createMockResultsApi } from "./mock";

let fallback: ResultsApi | null = null;
const Ctx = createContext<ResultsApi | null>(null);

export function ResultsApiProvider({ api, children }: { api: ResultsApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useResultsApi(): ResultsApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpResultsApi() : createMockResultsApi();
  return fallback;
}

export const rkeys = {
  scoped: (owner: string, what: string) => ["institution", "6c", owner, what] as const,
};

/** Result statuses a student may see (the API filters too). */
export const STUDENT_VISIBLE = ["published", "withdrawn"];
