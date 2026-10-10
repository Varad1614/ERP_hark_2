import { afterEach, describe, expect, it } from "vitest";
import { api, bootstrapLoginCsrf, getCsrfToken, oidcStartUrl, setCsrfToken } from "./api";

describe("API-mode CSRF handling", () => {
  afterEach(() => setCsrfToken(undefined));

  it("keeps the token from login / session responses in memory", () => {
    expect(getCsrfToken()).toBeUndefined();
    setCsrfToken("t-1");
    expect(getCsrfToken()).toBe("t-1");
    setCsrfToken(undefined);
    expect(getCsrfToken()).toBeUndefined();
  });

  it("sends X-CSRF-Token on unsafe requests only, and no scope headers", async () => {
    const seen: Request[] = [];
    const fetchStub = async (input: Request) => {
      seen.push(input);
      return new Response(null, { status: 204 });
    };
    setCsrfToken("t-2");
    await api.POST("/api/v1/auth/logout", { fetch: fetchStub, baseUrl: "http://localhost" });
    await api.GET("/api/v1/platform/modules", { fetch: fetchStub, baseUrl: "http://localhost" });
    expect(seen[0]?.headers.get("X-CSRF-Token")).toBe("t-2");
    expect(seen[0]?.headers.get("X-Sutra-Tenant-Id")).toBeNull();
    expect(seen[1]?.headers.get("X-CSRF-Token")).toBeNull();
  });
});

describe("login CSRF bootstrap", () => {
  afterEach(() => setCsrfToken(undefined));

  it("fetches GET /api/v1/auth/csrf and sends that token on the login request", async () => {
    const seen: Request[] = [];
    const fetchStub = async (input: Request) => {
      seen.push(input);
      if (input.url.endsWith("/api/v1/auth/csrf"))
        return Response.json({ csrfToken: "pre-1", expiresAt: "2030-01-01T00:00:00Z" });
      return Response.json({ outcome: "mfaRequired", challengeId: "c-1", mfaMethods: ["totp"], mfaEnrollmentRequired: false });
    };
    const token = await bootstrapLoginCsrf({ fetch: fetchStub, baseUrl: "http://localhost" });
    expect(token).toBe("pre-1");
    expect(getCsrfToken()).toBe("pre-1");
    expect(seen[0]?.method).toBe("GET");
    expect(seen[0]?.credentials).toBe("include");
    await api.POST("/api/v1/auth/login", {
      body: { loginId: "admin@sutra.dev", password: "sutra" },
      fetch: fetchStub,
      baseUrl: "http://localhost",
    });
    expect(seen[1]?.headers.get("X-CSRF-Token")).toBe("pre-1");
  });

  it("surfaces a problem response as an error and keeps no token", async () => {
    const fetchStub = async () =>
      new Response(JSON.stringify({ status: 503, errorCode: "dependency-unavailable", title: "x", type: "t", correlationId: "c" }), {
        status: 503,
        headers: { "content-type": "application/problem+json" },
      });
    await expect(bootstrapLoginCsrf({ fetch: fetchStub, baseUrl: "http://localhost" })).rejects.toThrow();
    expect(getCsrfToken()).toBeUndefined();
  });

  it("builds the OIDC start URL with a same-origin return path", () => {
    expect(oidcStartUrl("/workspaces")).toBe("/api/v1/auth/oidc/start?returnTo=%2Fworkspaces");
    expect(oidcStartUrl("/", true)).toContain("stepUp=true");
  });
});
