/**
 * React glue of the slice-8C library screens: API instance (context, mock or HTTP) and query
 * keys under the `institution` prefix, so `useCommand` (features/institution/hooks) refreshes
 * them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpLibraryApi, type LibraryApi } from "./api";
import { createMockLibraryApi } from "./mock";

let fallback: LibraryApi | null = null;
const Ctx = createContext<LibraryApi | null>(null);

export function LibraryApiProvider({ api, children }: { api: LibraryApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useLibraryApi(): LibraryApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpLibraryApi() : createMockLibraryApi();
  return fallback;
}

export const lkeys = (owner: string, what: string, extra = "") => ["institution", "8c-library", owner, what, extra] as const;
