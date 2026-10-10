import { afterEach, describe, expect, it } from "vitest";
import { api, getCsrfToken, setCsrfToken } from "./api";

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
