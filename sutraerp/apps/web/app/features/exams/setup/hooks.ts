/**
 * React glue of the slice-6A screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpExamSetupApi, type ExamSetupApi } from "./api";
import { createMockExamSetupApi } from "./mock";

let fallback: ExamSetupApi | null = null;
const Ctx = createContext<ExamSetupApi | null>(null);

export function ExamSetupApiProvider({ api, children }: { api: ExamSetupApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useExamSetupApi(): ExamSetupApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpExamSetupApi() : createMockExamSetupApi();
  return fallback;
}

export const ekeys = {
  scoped: (owner: string, what: string) => ["institution", "6a", owner, what] as const,
};
