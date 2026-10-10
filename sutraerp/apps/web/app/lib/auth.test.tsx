/**
 * API-mode sign-in against a stubbed sutra-api: login-CSRF bootstrap → login → MFA challenge →
 * verify (same pre-session token) → session CSRF token takes over.
 */
import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const principal = {
  subject: "dev|admin@sutra.dev",
  displayName: "Dhruv Naik",
  loginId: "admin@sutra.dev",
  roles: ["administrator"],
  scope: { tenantId: "0192f5a0-7c3e-7000-8000-000000000001" },
};

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("API-mode sign-in with MFA", () => {
  it("bootstraps login CSRF, answers the TOTP challenge and adopts the session token", async () => {
    vi.stubEnv("VITE_AUTH_MODE", "api");
    vi.stubEnv("VITE_API_BASE", "http://localhost");
    const seen: { method: string; path: string; csrf: string | null; body: string }[] = [];
    vi.stubGlobal("fetch", async (input: Request) => {
      const path = new URL(input.url).pathname;
      seen.push({ method: input.method, path, csrf: input.headers.get("X-CSRF-Token"), body: await input.clone().text() });
      switch (path) {
        case "/api/v1/auth/session":
          return Response.json({ status: 401, errorCode: "unauthenticated", title: "x", type: "t" }, { status: 401 });
        case "/api/v1/auth/csrf":
          return Response.json({ csrfToken: "pre-session", expiresAt: "2030-01-01T00:00:00Z" });
        case "/api/v1/auth/login":
          return Response.json({ outcome: "mfaRequired", challengeId: "ch-1", mfaMethods: ["totp"], mfaEnrollmentRequired: false });
        case "/api/v1/auth/mfa/verify":
          return Response.json({
            outcome: "authenticated",
            principal,
            csrfToken: "session-token",
            expiresAt: "2030-01-01T00:00:00Z",
            mfaLevel: "mfa",
            mfaEnrollmentRequired: false,
          });
        default:
          return new Response(null, { status: 204 });
      }
    });
    const { AuthProvider, useAuth } = await import("./auth");
    const { getCsrfToken } = await import("./api");
    let ctx: ReturnType<typeof useAuth> | undefined;
    function Probe() {
      ctx = useAuth();
      return <span data-testid="who">{ctx.session ? `${ctx.session.displayName}:${ctx.session.mfaLevel}` : "anon"}</span>;
    }
    render(
      <AuthProvider>
        <Probe />
      </AuthProvider>,
    );
    await waitFor(() => expect(ctx?.ready).toBe(true));

    let result: Awaited<ReturnType<NonNullable<typeof ctx>["signIn"]>> | undefined;
    await act(async () => {
      result = await ctx!.signIn("administrator");
    });
    expect(result).toEqual({ status: "mfaRequired", challengeId: "ch-1" });
    expect(screen.getByTestId("who").textContent).toBe("anon");
    const login = seen.find((r) => r.path === "/api/v1/auth/login");
    expect(seen.findIndex((r) => r.path === "/api/v1/auth/csrf")).toBeLessThan(seen.indexOf(login!));
    expect(login?.csrf).toBe("pre-session");

    await act(async () => {
      await ctx!.verifyMfa("ch-1", "123456");
    });
    const verify = seen.find((r) => r.path === "/api/v1/auth/mfa/verify");
    expect(verify?.csrf).toBe("pre-session");
    expect(JSON.parse(verify!.body)).toEqual({ challengeId: "ch-1", code: "123456" });
    expect(screen.getByTestId("who").textContent).toBe("Dhruv Naik:mfa");
    expect(getCsrfToken()).toBe("session-token");
  });
});
