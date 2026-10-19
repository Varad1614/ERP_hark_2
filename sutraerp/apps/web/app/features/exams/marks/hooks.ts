/**
 * React glue of the slice-6B screens: API instance (context, mock or HTTP) and query keys under
 * the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes them after
 * every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpMarksApi, type MarksApi } from "./api";
import { createMockMarksApi } from "./mock";

let fallback: MarksApi | null = null;
const Ctx = createContext<MarksApi | null>(null);

export function MarksApiProvider({ api, children }: { api: MarksApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useMarksApi(): MarksApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpMarksApi() : createMockMarksApi();
  return fallback;
}

export const mkeys = (owner: string, what: string) => ["institution", "6b", owner, what] as const;
