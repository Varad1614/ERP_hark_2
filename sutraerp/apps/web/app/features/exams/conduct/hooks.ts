/**
 * React glue of the conduct screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpExamConductApi, type ExamConductApi } from "./api";
import { createMockExamConductApi } from "./mock";

let fallback: ExamConductApi | null = null;
const Ctx = createContext<ExamConductApi | null>(null);

export function ExamConductApiProvider({ api, children }: { api: ExamConductApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useExamConductApi(): ExamConductApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpExamConductApi() : createMockExamConductApi();
  return fallback;
}

export const ckeys = {
  scoped: (owner: string, what: string) => ["institution", "6a-conduct", owner, what] as const,
};
