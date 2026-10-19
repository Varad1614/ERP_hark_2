/**
 * Sign-in second-factor step in API mode: the passkey option appears when the challenge offers
 * WebAuthn, runs the ceremony and lands on home; TOTP stays available.
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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

async function mount(methods: string[]) {
  vi.stubEnv("VITE_AUTH_MODE", "api");
  vi.stubEnv("VITE_API_BASE", "http://localhost");
  const paths: string[] = [];
  vi.stubGlobal("fetch", async (input: Request) => {
    const path = new URL(input.url).pathname;
    paths.push(path);
    switch (path) {
      case "/api/v1/auth/session":
        return Response.json({ status: 401, errorCode: "unauthenticated", title: "x", type: "t" }, { status: 401 });
      case "/api/v1/auth/csrf":
        return Response.json({ csrfToken: "pre", expiresAt: "2030-01-01T00:00:00Z" });
      case "/api/v1/auth/login":
        return Response.json({ outcome: "mfaRequired", challengeId: "ch-1", mfaMethods: methods, mfaEnrollmentRequired: false });
      case "/api/v1/auth/mfa/webauthn/challenge":
        return Response.json({ challengeId: "ch-2", options: { publicKey: { challenge: "AQID", rpId: "localhost" } } });
      case "/api/v1/auth/mfa/webauthn/verify":
        return Response.json({ outcome: "authenticated", principal, csrfToken: "s", expiresAt: "2030-01-01T00:00:00Z", mfaLevel: "mfa", mfaEnrollmentRequired: false });
      default:
        return new Response(null, { status: 204 });
    }
  });
  const buf = (...b: number[]) => new Uint8Array(b).buffer;
  const get = vi.fn(async () => ({
    id: "cred",
    rawId: buf(1),
    type: "public-key",
    response: { clientDataJSON: buf(1), authenticatorData: buf(2), signature: buf(3), userHandle: null },
  }));
  vi.stubGlobal("PublicKeyCredential", function PublicKeyCredential() {});
  vi.stubGlobal("navigator", { ...navigator, credentials: { get } });
  const { AuthProvider } = await import("../lib/auth");
  const { default: SignIn } = await import("./sign-in");
  const { createMemoryRouter, RouterProvider } = await import("react-router");
  // Same module instances as the screen (the registry is reset between tests).
  const { ToastProvider } = await import("@sutra/ui");
  const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
  const router = createMemoryRouter(
    [
      { path: "/sign-in", element: <SignIn /> },
      { path: "/", element: <p>home</p> },
    ],
    { initialEntries: ["/sign-in"] },
  );
  render(
    <QueryClientProvider client={new QueryClient()}>
      <AuthProvider>
        <ToastProvider>
          <RouterProvider router={router} />
        </ToastProvider>
      </AuthProvider>
    </QueryClientProvider>,
  );
  fireEvent.change(await screen.findByLabelText("Password"), { target: { value: "typed-dev-password" } });
  fireEvent.click(await screen.findByRole("button", { name: /Continue as administrator/ }));
  await screen.findByText("Verify it’s you");
  return { router, get, paths };
}

describe("sign-in second factor", () => {
  it("offers a passkey next to the authenticator code and signs in with it", async () => {
    const { router, get, paths } = await mount(["webauthn", "totp"]);
    expect(screen.getByLabelText("Authentication code")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Use a passkey" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(get).toHaveBeenCalledTimes(1);
    expect(paths).toContain("/api/v1/auth/mfa/webauthn/verify");
  });

  it("shows only the code when the account has no passkey", async () => {
    await mount(["totp"]);
    expect(screen.queryByRole("button", { name: "Use a passkey" })).toBeNull();
    expect(screen.getByLabelText("Authentication code")).toBeTruthy();
  });
});
