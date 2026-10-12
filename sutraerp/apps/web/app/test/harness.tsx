/**
 * Test harness for screens: query client, auth (mock mode, persona from sessionStorage), toasts
 * and an in-memory router. `renderScreen` resolves once the session is ready.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ToastProvider } from "@sutra/ui";
import { render, screen, waitFor } from "@testing-library/react";
import { expect } from "vitest";
import type { ReactNode } from "react";
import { createMemoryRouter, Outlet, RouterProvider } from "react-router";
import { AuthProvider, useAuth } from "../lib/auth";
import type { Role } from "../lib/roles";

function Gate({ children }: { children: ReactNode }) {
  const { ready, session } = useAuth();
  if (!ready || !session) return <span data-testid="not-ready" />;
  return <>{children}</>;
}

/**
 * Renders `element` at route `path` signed in as `role` (mock mode); other paths render a marker.
 * `entry` is the initial URL when `path` is a pattern (e.g. `path: "/campus/:tab?"`, `entry: "/campus"`).
 */
export async function renderScreen(element: ReactNode, { role = "administrator", path = "/", entry }: { role?: Role; path?: string; entry?: string } = {}) {
  window.sessionStorage.setItem("sutra.devRole", role);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 60_000 } } });
  const router = createMemoryRouter(
    [
      {
        element: (
          <Gate>
            <Outlet />
          </Gate>
        ),
        children: [
          { path, element },
          { path: "*", element: <p data-testid="navigated">elsewhere</p> },
        ],
      },
    ],
    { initialEntries: [entry ?? path] },
  );
  const utils = render(
    <QueryClientProvider client={client}>
      <AuthProvider>
        <ToastProvider>
          <RouterProvider router={router} />
        </ToastProvider>
      </AuthProvider>
    </QueryClientProvider>,
  );
  await waitFor(() => expect(screen.queryByTestId("not-ready")).toBeNull());
  return { ...utils, router, client };
}
