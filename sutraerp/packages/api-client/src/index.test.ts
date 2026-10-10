import { describe, expect, it } from "vitest";
import { CSRF_HEADER, createSutraClient, csrfMiddleware, isProblem, isUnsafeMethod, newIdempotencyKey } from "./index";

describe("api-client helpers", () => {
  it("detects problem documents", () => {
    expect(isProblem({ type: "urn:sutraerp:problem:x", title: "x", status: 400, errorCode: "x" })).toBe(true);
    expect(isProblem({ items: [] })).toBe(false);
  });
  it("creates unique idempotency keys", () => {
    expect(newIdempotencyKey()).not.toEqual(newIdempotencyKey());
  });

});

describe("CSRF", () => {
  async function run(method: string, token: string | undefined) {
    const mw = csrfMiddleware(() => token);
    const request = new Request("http://localhost/api/v1/x", { method });
    // openapi-fetch passes more context; the middleware only reads `request`.
    const out = await mw.onRequest!({ request } as Parameters<NonNullable<typeof mw.onRequest>>[0]);
    return (out as Request).headers.get(CSRF_HEADER);
  }

  it("classifies unsafe methods", () => {
    for (const m of ["POST", "put", "PATCH", "DELETE"]) expect(isUnsafeMethod(m)).toBe(true);
    for (const m of ["GET", "HEAD", "OPTIONS"]) expect(isUnsafeMethod(m)).toBe(false);
  });

  it("adds the token to unsafe requests only", async () => {
    expect(await run("POST", "tok")).toBe("tok");
    expect(await run("DELETE", "tok")).toBe("tok");
    expect(await run("GET", "tok")).toBeNull();
    expect(await run("POST", undefined)).toBeNull();
  });

  it("client sends X-CSRF-Token and never scope headers", async () => {
    const seen: Request[] = [];
    const fetchStub = async (input: Request) => {
      seen.push(input);
      return new Response(null, { status: 204 });
    };
    const client = createSutraClient({ baseUrl: "http://localhost", csrfToken: () => "abc" });
    await client.POST("/api/v1/auth/logout", { fetch: fetchStub });
    await client.GET("/api/v1/platform/modules", { fetch: fetchStub });
    expect(seen[0]?.headers.get(CSRF_HEADER)).toBe("abc");
    expect(seen[0]?.headers.get("X-Sutra-Tenant-Id")).toBeNull();
    expect(seen[1]?.headers.get(CSRF_HEADER)).toBeNull();
  });
});
