/**
 * Part 2 · S7-entitle: module entitlements in the client — what to OFFER (navigation pills, Ctrl K
 * palette entries, workspace tiles, routes) and the licence banner. Never the control: the API
 * refuses every request to an inactive module (`403 entitlement.module_inactive`), writes after
 * expiry (`entitlement.licence_expired`) and everything but export during a suspension
 * (`entitlement.licence_suspended`) — `crates/web/src/entitlement.rs`.
 *
 * The registry is the shared `catalog/entitlements.json` (the same file the API embeds); its
 * `web` section maps the first route segment and workspace keys onto module keys.
 */
import type { EffectiveEntitlement } from "@sutra/api-client";
import { useQuery } from "@tanstack/react-query";
import registryJson from "../../../../catalog/entitlements.json";
import { api, AUTH_MODE, unwrap, USES_API } from "./api";
import { routeFor, type NavEntry } from "./roles";

export type { EffectiveEntitlement };

export interface RegistryModule {
  key: string;
  name: string;
  crate: string;
  activatable: boolean;
  requires: string[];
  description: string;
}

export interface RegistryPackage {
  key: string;
  name: string;
  description: string;
  modules: string[];
}

interface Registry {
  modules: RegistryModule[];
  packages: RegistryPackage[];
  defaultPackage: string;
  web: { routes: Record<string, string>; workspaces: Record<string, string> };
}

/** Shared copy of `catalog/entitlements.json`. */
export const REGISTRY = registryJson as unknown as Registry;

const STRIP_COMMENTS = (o: Record<string, string>) => Object.fromEntries(Object.entries(o).filter(([k]) => !k.startsWith("$")));
const ROUTES = STRIP_COMMENTS(REGISTRY.web.routes);
const WORKSPACES = STRIP_COMMENTS(REGISTRY.web.workspaces);

/** Module of a workspace key (`W-E06` → `payroll`; longest prefix wins), `null` = core. */
export function moduleForWorkspace(key: string): string | null {
  let best: string | null = null;
  let len = 0;
  for (const [prefix, module] of Object.entries(WORKSPACES)) {
    if (key.startsWith(prefix) && prefix.length > len) {
      best = module;
      len = prefix.length;
    }
  }
  return best;
}

/** Module a client route belongs to (`/finance/fees` → `fees`), `null` = core. */
export function moduleForRoute(pathname: string): string | null {
  const [first = "", second] = pathname.split("/").filter(Boolean);
  if (first === "workspaces") return second ? moduleForWorkspace(second) : null;
  return ROUTES[first] ?? null;
}

/** Display name of a module key. */
export function moduleName(key: string): string {
  return REGISTRY.modules.find((m) => m.key === key)?.name ?? key;
}

/**
 * True when `module` is active. Core (`null`) is always active; while the entitlement is still
 * loading everything is offered (the API remains the control).
 */
export function isModuleActive(ent: Pick<EffectiveEntitlement, "modules"> | undefined, module: string | null): boolean {
  if (!module || !ent) return true;
  return ent.modules.includes(module);
}

/** Navigation entries of active modules only. */
export function visibleNav<T extends Pick<NavEntry, "id">>(nav: readonly T[], ent: Pick<EffectiveEntitlement, "modules"> | undefined): T[] {
  return nav.filter((n) => isModuleActive(ent, moduleForRoute(routeFor(n.id))));
}

/** Workspaces of active modules only. */
export function visibleWorkspaces<T extends { key: string }>(items: readonly T[], ent: Pick<EffectiveEntitlement, "modules"> | undefined): T[] {
  return items.filter((w) => isModuleActive(ent, moduleForWorkspace(w.key)));
}

/** Mock mode: the default every tenant without a stored entitlement gets (perpetual, active). */
export function defaultEntitlement(now = new Date()): EffectiveEntitlement {
  const pkg = REGISTRY.packages.find((p) => p.key === REGISTRY.defaultPackage);
  return {
    package: REGISTRY.defaultPackage,
    addOnModules: [],
    modules: [...(pkg?.modules ?? [])].sort(),
    status: "active",
    storedStatus: "active",
    validFrom: new Date(0).toISOString(),
    validUntil: null,
    graceDays: 0,
    graceEndsAt: null,
    daysRemaining: null,
    readOnly: false,
    origin: "default",
    version: 0,
    asOf: now.toISOString(),
  };
}

export const entitlementKey = ["platform", "entitlement", AUTH_MODE] as const;

/** The session tenant's entitlement (`GET /api/v1/platform/entitlement`), re-read every minute. */
export function useEntitlement(enabled = true) {
  return useQuery({
    queryKey: entitlementKey,
    queryFn: async (): Promise<EffectiveEntitlement> => (USES_API ? unwrap(await api.GET("/api/v1/platform/entitlement")) : defaultEntitlement()),
    enabled,
    staleTime: 60_000,
    refetchInterval: 5 * 60_000,
  });
}

export interface LicenceBannerModel {
  status: EffectiveEntitlement["status"] | "renewal_due";
  tone: "amber" | "rose" | "blue";
  badge: "attention" | "review" | "info";
  title: string;
  detail: string;
}

const day = (iso: string | null | undefined) => (iso ? new Date(iso).toLocaleDateString("en-IN", { day: "numeric", month: "short", year: "numeric" }) : "");
const days = (n: number) => `${n} day${n === 1 ? "" : "s"}`;

/** The banner the shell shows for a licence that needs attention, `null` when none. */
export function licenceBanner(ent: EffectiveEntitlement | undefined): LicenceBannerModel | null {
  if (!ent) return null;
  switch (ent.status) {
    case "grace":
      return {
        status: "grace",
        tone: "amber",
        badge: "attention",
        title: "Licence renewal due",
        detail: `Your SutraERP licence ended${ent.validUntil ? ` on ${day(ent.validUntil)}` : ""}. Full access continues${ent.daysRemaining != null ? ` for ${days(Math.max(0, ent.daysRemaining))}` : ""}${ent.graceEndsAt ? ` (until ${day(ent.graceEndsAt)})` : ""}; after that the ERP becomes read-only until it is renewed.`,
      };
    case "expired":
      return {
        status: "expired",
        tone: "rose",
        badge: "attention",
        title: "Licence expired — read-only",
        detail: "You can still view and export your institution's data. Changes are paused until a platform administrator renews the licence.",
      };
    case "suspended":
      return {
        status: "suspended",
        tone: "rose",
        badge: "attention",
        title: "Licence suspended",
        detail: "Only platform administration and data export are available until the licence is reinstated.",
      };
    case "active":
      if (ent.daysRemaining != null && ent.daysRemaining <= 30) {
        return {
          status: "renewal_due",
          tone: "blue",
          badge: "info",
          title: "Licence renews soon",
          detail: `Your SutraERP licence runs until ${day(ent.validUntil)} (${days(Math.max(0, ent.daysRemaining))} left).`,
        };
      }
      return null;
    default:
      return null;
  }
}
