import { useQuery } from "@tanstack/react-query";
import { ApiProblem, createSutraClient, isProblem, type Workspace, type WorkspaceList } from "@sutra/api-client";
import catalogJson from "../../../../catalog/workspaces.json";
import type { Role } from "./roles";

/** `mock`: everything resolved in the browser. `api`: talk to sutra-api (same origin). */
export const AUTH_MODE: "mock" | "api" = import.meta.env.VITE_AUTH_MODE === "api" ? "api" : "mock";

/**
 * Synchronizer CSRF token of the current server-side session (ADR-L5-0038). Kept in memory only
 * (never in storage): it is re-read from `GET /api/v1/auth/session` on every page load and sent
 * as `X-CSRF-Token` on POST/PUT/PATCH/DELETE. Tenant scope is never sent; the API takes it from
 * the session.
 */
let csrfToken: string | undefined;
export function setCsrfToken(token: string | undefined) {
  csrfToken = token;
}
export function getCsrfToken(): string | undefined {
  return csrfToken;
}

export const api = createSutraClient({ baseUrl: import.meta.env.VITE_API_BASE ?? "", csrfToken: getCsrfToken });

/** Shared copy of `catalog/workspaces.json` (the same file the API embeds). */
export const CATALOG = catalogJson as unknown as Workspace[];

export function filterCatalog(role: Role, catalog: Workspace[] = CATALOG): WorkspaceList {
  const items = catalog
    .filter((w) => w.roles.includes(role))
    .map((w) => ({ ...w, workflows: w.workflows.filter((f) => f.roles.includes(role)) }));
  return { items, total: items.length, workflowTotal: items.reduce((n, w) => n + w.workflows.length, 0) };
}

export function unwrap<T>(result: { data?: T; error?: unknown }): T {
  if (result.error !== undefined) {
    if (isProblem(result.error)) throw new ApiProblem(result.error);
    throw new Error("Request failed");
  }
  if (result.data === undefined) throw new Error("Empty response");
  return result.data;
}

export function useWorkspaces(role: Role) {
  return useQuery({
    queryKey: ["platform", "workspaces", role, AUTH_MODE],
    queryFn: async (): Promise<WorkspaceList> =>
      AUTH_MODE === "api" ? unwrap(await api.GET("/api/v1/platform/workspaces", { params: { query: { role } } })) : filterCatalog(role),
  });
}
