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
export type MfaLevel = Schemas["MfaLevel"];
export type MfaMethod = Schemas["MfaMethod"];
export type CsrfTokenResponse = Schemas["CsrfTokenResponse"];
export type TotpEnrolmentResponse = Schemas["TotpEnrolmentResponse"];
export type LogoutResponse = Schemas["LogoutResponse"];
// Batch 1D
export type Notification = Schemas["Notification"];
export type NotificationPage = Schemas["NotificationPage"];
export type NotificationCategory = Schemas["NotificationCategory"];
export type Severity = Schemas["Severity"];
export type Notice = Schemas["Notice"];
export type NoticeList = Schemas["NoticeList"];
export type NoticeState = Schemas["NoticeState"];
export type NewNotice = Schemas["NewNotice"];
export type UpdateNotice = Schemas["UpdateNotice"];
export type Todo = Schemas["Todo"];
export type TodoPage = Schemas["TodoPage"];
export type TodoStatus = Schemas["TodoStatus"];
export type NewTodo = Schemas["NewTodo"];
export type Profile = Schemas["Profile"];
export type UpdateProfile = Schemas["UpdateProfile"];
export type CategoryPreference = Schemas["CategoryPreference"];
export type MfaStatus = Schemas["MfaStatus"];
export type PasskeyInfo = Schemas["PasskeyInfo"];
export type SessionView = Schemas["SessionView"];
export type WebauthnCeremony = Schemas["WebauthnCeremony"];
export type WebauthnChallengeResponse = Schemas["WebauthnChallengeResponse"];
// Part 2 · S7-entitle: module entitlements + licence state
export type EffectiveEntitlement = Schemas["EffectiveEntitlement"];
export type LicenceStatus = Schemas["LicenceStatus"];
export type EntitlementOrigin = Schemas["EntitlementOrigin"];
export type EntitlementCatalogue = Schemas["EntitlementCatalogue"];
export type EntitlementRequestView = Schemas["EntitlementRequestView"];

export interface ClientOptions {
  /** Origin of the API; empty string = same origin (default, via dev proxy / ingress). */
  baseUrl?: string;
  /**
   * Current CSRF token, sent as `X-CSRF-Token` on every unsafe request: before sign-in the
   * pre-session token from `GET /api/v1/auth/csrf` (login + MFA verify), afterwards the
   * session's synchronizer token (from login or `GET /api/v1/auth/session`). Tenant/institution
   * scope is never sent: the server takes it from the session (ADR-L5-0004).
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

/**
 * Human sentence for an HTTP status (audit 2026-10 Phase B: never surface "Request failed (422)").
 * Used when a response carries no problem `detail`, or only a client-made "Request failed (n)" title.
 */
export function statusMessage(status: number): string {
  if (status === 400) return "The request could not be read. Check the form and try again.";
  if (status === 401) return "Your session has ended. Sign in again to continue.";
  if (status === 403) return "You don't have permission to do this.";
  if (status === 404) return "That record could not be found. It may have been removed.";
  if (status === 409 || status === 412) return "This record was changed by someone else. Refresh and try again.";
  if (status === 413) return "That is too large to send.";
  if (status === 415) return "That kind of content is not accepted here.";
  if (status === 422) return "Some details are not valid. Check the highlighted fields.";
  if (status === 429) return "Too many requests. Wait a moment and try again.";
  if (status >= 500) return "The server could not complete this right now. Try again shortly.";
  return "Something went wrong. Try again.";
}

/** Client-made placeholder titles (`Request failed (422)`) that must not reach the user. */
export const GENERIC_TITLE = /^Request failed(?: \(\d+\))?$/;

/** Error thrown for RFC 9457 problem responses. */
export class ApiProblem extends Error {
  constructor(public readonly problem: Problem) {
    super(problem.detail || (!problem.title || GENERIC_TITLE.test(problem.title) ? statusMessage(problem.status) : problem.title));
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
