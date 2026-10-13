/**
 * React glue of the slice-4B screens: API instance (context, mock or HTTP) and query keys.
 * Keys live under the `institution` prefix so `useCommand` (features/institution/hooks)
 * refreshes them after every command.
 */
import { createContext, createElement, useContext, type ReactNode } from "react";
import { USES_API } from "../../../lib/api";
import { createHttpDeliveryApi, type DeliveryApi } from "./api";
import { createMockDeliveryApi } from "./mock";

let fallback: DeliveryApi | null = null;
const Ctx = createContext<DeliveryApi | null>(null);

export function DeliveryApiProvider({ api, children }: { api: DeliveryApi; children: ReactNode }) {
  return createElement(Ctx.Provider, { value: api }, children);
}

export function useDeliveryApi(): DeliveryApi {
  const api = useContext(Ctx);
  if (api) return api;
  fallback ??= USES_API ? createHttpDeliveryApi() : createMockDeliveryApi();
  return fallback;
}

export const dkeys = {
  scoped: (owner: string, what: string) => ["institution", "4b", owner, what] as const,
};

/** ISO day number (1 = Monday) → short name. */
export const DAY = ["", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;
