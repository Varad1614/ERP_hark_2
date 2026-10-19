/** Slice 3C React glue: API instance (context → mock or HTTP) and query keys. */
import { createContext, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpC3Api, type C3Api } from "./api";
import { createMockC3Api } from "./mock";

let fallback: C3Api | null = null;
const Ctx = createContext<C3Api | null>(null);

export function C3ApiProvider({ api, children }: { api: C3Api; children: ReactNode }) {
  return <Ctx.Provider value={api}>{children}</Ctx.Provider>;
}

export function useC3Api(): C3Api {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpC3Api() : createMockC3Api();
  return fallback;
}

/** Keys live under `institution` so `useCommand` refetches them after every command. */
export const k3 = (...parts: string[]) => ["institution", "3c", ...parts] as const;
