/**
 * React glue of Admin → Data import: the API instance (context, mock or HTTP) and query keys
 * under the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them
 * after every command (and opens the step-up dialog the approval needs).
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../lib/api";
import { createHttpImportApi, type ImportApi } from "./api";
import { createMockImportApi } from "./mock";

let fallback: ImportApi | null = null;
const Ctx = createContext<ImportApi | null>(null);

export function ImportApiProvider({ api, children }: { api: ImportApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useImportApi(): ImportApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpImportApi() : createMockImportApi();
  return fallback;
}

export const ikeys = {
  entities: () => ["institution", "imports", "entities"] as const,
  profiles: (entity: string) => ["institution", "imports", "profiles", entity] as const,
  jobs: (institutionId: string) => ["institution", "imports", "jobs", institutionId] as const,
  job: (id: string) => ["institution", "imports", "job", id] as const,
  rows: (id: string, errorsOnly: boolean) => ["institution", "imports", "rows", id, errorsOnly ? "errors" : "all"] as const,
};
