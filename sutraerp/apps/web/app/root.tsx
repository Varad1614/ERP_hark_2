import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ToastProvider } from "@sutra/ui";
import "@sutra/ui/styles.css";
import "./layout-utilities.css";
import { useState, type ReactNode } from "react";
import { isRouteErrorResponse, Links, Meta, Outlet, Scripts, ScrollRestoration, useRouteError } from "react-router";
import { AuthProvider } from "./lib/auth";

export function Layout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <meta name="theme-color" content="#FAF8F4" />
        <title>SutraERP</title>
        <Meta />
        <Links />
      </head>
      <body>
        {children}
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}

export default function App() {
  const [queryClient] = useState(
    () => new QueryClient({ defaultOptions: { queries: { staleTime: 60_000, retry: 1, refetchOnWindowFocus: false } } }),
  );
  return (
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <ToastProvider>
          <Outlet />
        </ToastProvider>
      </AuthProvider>
    </QueryClientProvider>
  );
}

export function HydrateFallback() {
  return (
    <div className="sb-app" aria-busy="true">
      <div className="sb-logo">
        <i aria-hidden="true" />
        sutra
      </div>
    </div>
  );
}

export function ErrorBoundary() {
  const error = useRouteError();
  const title = isRouteErrorResponse(error) ? `${error.status} ${error.statusText}` : "Something went wrong";
  return (
    <div className="sb-app">
      <div className="sb-grid">
        <div className="sb-tile sb-tile--accent sb-soon">
          <span className="sb-chip">SutraERP</span>
          <h1>{title}</h1>
          <p>Try going back to your home dashboard.</p>
          <div className="sb-actions-row">
            <a className="sb-btn" href="/">
              Back to home
            </a>
          </div>
        </div>
      </div>
    </div>
  );
}
