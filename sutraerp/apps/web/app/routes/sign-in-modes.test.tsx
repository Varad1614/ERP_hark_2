/**
 * Audit 2026-10 (A1 · D1/C1/M10/SEC-2/SEC-7/M-3): what the sign-in page offers per build.
 *
 * - A build without VITE_AUTH_MODE is `oidc` (real sign-in) — never `mock`.
 * - `oidc`: a visible "Sign in" button that starts GET /api/v1/auth/oidc/start; no personas.
 * - `api` without VITE_DEV_PERSONAS: a login id + password form only; no persona cards, no dev
 *   login ids in the client.
 * - `api` with VITE_DEV_PERSONAS=true: persona cards too, but the password is always typed.
 * - No dev password ships in the client source.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => {
  vi.unstubAllEnvs();
  vi.unstubAllGlobals();
  vi.resetModules();
});

/** A production-like build: not the dev server. */
function prodBuild(mode: string | undefined, personas?: "true") {
  vi.stubEnv("DEV", false);
  vi.stubEnv("PROD", true);
  if (mode !== undefined) vi.stubEnv("VITE_AUTH_MODE", mode);
  else vi.stubEnv("VITE_AUTH_MODE", "");
  vi.stubEnv("VITE_DEV_PERSONAS", personas ?? "");
  vi.stubEnv("VITE_API_BASE", "http://localhost");
}

async function mount(initial = "/sign-in") {
  const bodies: { path: string; body: string }[] = [];
  vi.stubGlobal("fetch", async (input: Request) => {
    const p = new URL(input.url).pathname;
    bodies.push({ path: p, body: await input.clone().text() });
    if (p === "/api/v1/auth/csrf") return Response.json({ csrfToken: "pre", expiresAt: "2030-01-01T00:00:00Z" });
    if (p === "/api/v1/auth/login") return Response.json({ outcome: "mfaRequired", challengeId: "ch-1", mfaMethods: ["totp"], mfaEnrollmentRequired: false });
    return Response.json({ status: 401, errorCode: "unauthenticated", title: "x", type: "t" }, { status: 401 });
  });
  const { AuthProvider } = await import("../lib/auth");
  const { default: SignIn } = await import("./sign-in");
  const { createMemoryRouter, RouterProvider } = await import("react-router");
  const { ToastProvider } = await import("@sutra/ui");
  const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
  const router = createMemoryRouter([{ path: "/sign-in", element: <SignIn /> }], { initialEntries: [initial] });
  render(
    <QueryClientProvider client={new QueryClient()}>
      <AuthProvider>
        <ToastProvider>
          <RouterProvider router={router} />
        </ToastProvider>
      </AuthProvider>
    </QueryClientProvider>,
  );
  await screen.findByRole("heading", { name: /Sign in to SutraERP/ });
  return { bodies };
}

describe("sign-in per build", () => {
  it("defaults a build without VITE_AUTH_MODE to oidc, never mock", async () => {
    prodBuild(undefined);
    const { AUTH_MODE } = await import("../lib/api");
    expect(AUTH_MODE).toBe("oidc");
  });

  it("oidc: a visible Sign in button that starts single sign-on, and no personas", async () => {
    prodBuild("oidc");
    const assign = vi.fn();
    vi.stubGlobal("location", { ...window.location, assign });
    await mount("/sign-in?returnTo=%2Ffinance%3Ftab%3Dfees");
    expect(document.querySelector("[data-role-card]")).toBeNull();
    expect(document.querySelector("[data-password]")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(assign).toHaveBeenCalledWith("http://localhost/api/v1/auth/oidc/start?returnTo=%2Ffinance%3Ftab%3Dfees");
  });

  it("oidc: an off-site returnTo falls back to /", async () => {
    const { safeReturnTo } = await import("../lib/api");
    for (const bad of ["//evil.example", "https://evil.example", "/\\evil.example", "evil", "/sign-in?x=1", null]) expect(safeReturnTo(bad)).toBe("/");
    expect(safeReturnTo("/students/admissions?x=1")).toBe("/students/admissions?x=1");
  });

  it("api without VITE_DEV_PERSONAS: login id + password form only, no persona picker", async () => {
    prodBuild("api");
    const { devPersonaLoginId, devPersonasEnabled } = await import("../lib/dev-personas");
    expect(devPersonasEnabled()).toBe(false);
    expect(devPersonaLoginId("administrator")).toBeUndefined();
    const { bodies } = await mount();
    expect(document.querySelector("[data-role-card]")).toBeNull();
    fireEvent.change(screen.getByLabelText("Login id"), { target: { value: "registrar@college.edu" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "what-the-user-typed" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    await screen.findByText("Verify it’s you");
    const login = bodies.find((b) => b.path === "/api/v1/auth/login");
    expect(JSON.parse(login!.body)).toEqual({ loginId: "registrar@college.edu", password: "what-the-user-typed" });
  });

  it("api with VITE_DEV_PERSONAS=true: persona cards, password still typed", async () => {
    prodBuild("api", "true");
    const { bodies } = await mount();
    const card = document.querySelector('[data-role-card="faculty"]') as HTMLButtonElement;
    expect(card).toBeTruthy();
    fireEvent.click(card);
    expect(await screen.findByText("Type the development password first.")).toBeTruthy();
    expect(bodies.some((b) => b.path === "/api/v1/auth/login")).toBe(false);
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "typed-dev-password" } });
    fireEvent.click(card);
    await waitFor(() => expect(bodies.some((b) => b.path === "/api/v1/auth/login")).toBe(true));
    const login = bodies.find((b) => b.path === "/api/v1/auth/login");
    expect(JSON.parse(login!.body)).toEqual({ loginId: "meera.shah@sutra.dev", password: "typed-dev-password" });
  });
});

describe("no dev password ships", () => {
  const appDir = path.resolve(__dirname, "..");
  const files = (dir: string): string[] =>
    readdirSync(dir).flatMap((f) => {
      const p = path.join(dir, f);
      if (statSync(p).isDirectory()) return files(p);
      return /\.(tsx?|mjs)$/.test(f) && !/\.test\.tsx?$/.test(f) ? [p] : [];
    });

  it("client source never embeds the dev password or reads VITE_DEV_PASSWORD", () => {
    for (const f of files(appDir)) {
      const src = readFileSync(f, "utf8");
      expect(src, f).not.toMatch(/password\s*:\s*["'`]sutra["'`]|\?\?\s*["'`]sutra["'`]/);
      expect(src, f).not.toContain("VITE_DEV_PASSWORD");
    }
  });

  it("a built bundle (when present) contains no dev password and, without dev personas, no persona login ids", () => {
    const dist = path.resolve(appDir, "../build/client/assets");
    let js: string[] = [];
    try {
      js = readdirSync(dist).filter((f) => f.endsWith(".js"));
    } catch {
      return; // no build in this checkout (CI builds before the E2E jobs, not before Vitest)
    }
    const bundle = js.map((f) => readFileSync(path.join(dist, f), "utf8")).join("\n");
    expect(bundle).not.toMatch(/password:\s*"sutra"|\?\?\s*"sutra"/);
    if (process.env.VITE_DEV_PERSONAS !== "true" && !process.env.VITE_UI_CATALOGUE) {
      expect(bundle).not.toContain("aarav.desai@sutra.dev");
    }
  });
});
