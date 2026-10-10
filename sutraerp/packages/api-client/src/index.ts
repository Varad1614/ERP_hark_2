/**
 * Typed SutraERP API client.
 *
 * `schema.d.ts` is generated from `openapi/openapi.json` (which is itself generated from the Rust
 * code by utoipa) — run `pnpm gen:api` after changing any API. CI fails if either drifts.
 */
import createClient, { type Middleware } from "openapi-fetch";
import type { components, paths } from "./schema";

export type { components, paths } from "./schema";

type Schemas = components["schemas"];
export type Principal = Schemas["Principal"];
export type Role = Schemas["Role"];
export type Workspace = Schemas["Workspace"];
export type WorkflowEntry = Schemas["WorkflowEntry"];
export type WorkspaceList = Schemas["WorkspaceList"];
export type LoginRequest = Schemas["LoginRequest"];
export type LoginResponse = Schemas["LoginResponse"];
export type SessionInfo = Schemas["SessionInfo"];
export type ModuleDescriptor = Schemas["ModuleDescriptor"];
export type Problem = Schemas["Problem"];

export interface ClientOptions {
  /** Origin of the API; empty string = same origin (default, via dev proxy / ingress). */
  baseUrl?: string;
  /**
   * Current session's synchronizer CSRF token (from login or `GET /api/v1/auth/session`).
   * Sent as `X-CSRF-Token` on every unsafe request. Tenant/institution scope is never sent: the
   * server takes it from the session (ADR-L5-0004).
   */
  csrfToken?: () => string | undefined;
}

/** Header carrying the session's CSRF token (ADR-L5-0038). */
export const CSRF_HEADER = "X-CSRF-Token";

const UNSAFE_METHODS = new Set(["POST", "PUT", "PATCH", "DELETE"]);

/** True for methods that change state and must carry the CSRF token. */
export function isUnsafeMethod(method: string): boolean {
  return UNSAFE_METHODS.has(method.toUpperCase());
}

/** Adds `X-CSRF-Token` to unsafe requests when a token is known. */
export function csrfMiddleware(csrfToken: () => string | undefined): Middleware {
  return {
    onRequest({ request }) {
      if (isUnsafeMethod(request.method)) {
        const token = csrfToken();
        if (token) request.headers.set(CSRF_HEADER, token);
      }
      return request;
    },
  };
}

/** Error thrown for RFC 9457 problem responses. */
export class ApiProblem extends Error {
  constructor(public readonly problem: Problem) {
    super(problem.detail ?? problem.title);
    this.name = "ApiProblem";
  }
}

export function isProblem(value: unknown): value is Problem {
  return typeof value === "object" && value !== null && "errorCode" in value && "status" in value;
}

/** New UUIDv4 for `Idempotency-Key` headers on retry-sensitive commands. */
export function newIdempotencyKey(): string {
  return crypto.randomUUID();
}

export function createSutraClient(options: ClientOptions = {}) {
  const client = createClient<paths>({ baseUrl: options.baseUrl ?? "", credentials: "include" });
  if (options.csrfToken) client.use(csrfMiddleware(options.csrfToken));
  return client;
}

export type SutraClient = ReturnType<typeof createSutraClient>;
