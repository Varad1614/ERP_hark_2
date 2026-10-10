/**
 * Client session state. The server-side session cookie is HttpOnly (ADR-L5-0038), so in `api`
 * mode the browser only ever learns the principal (and the session's CSRF token) through the
 * login response and `GET /api/v1/auth/session`.
 * In `mock` mode (default for UI work) the chosen dev persona is kept in sessionStorage.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { AUTH_MODE, api, setCsrfToken, unwrap } from "./api";
import { isRole, ROLE_CONFIG, type Role } from "./roles";

export interface ClientSession {
  role: Role;
  displayName: string;
  loginId: string;
}

interface AuthContextValue {
  ready: boolean;
  session: ClientSession | null;
  signIn: (role: Role) => Promise<void>;
  signOut: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue | null>(null);
const STORAGE_KEY = "sutra.devRole";

function personaSession(role: Role): ClientSession {
  const c = ROLE_CONFIG[role];
  return { role, displayName: c.name, loginId: c.loginId };
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [session, setSession] = useState<ClientSession | null>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (AUTH_MODE === "mock") {
        const stored = window.sessionStorage.getItem(STORAGE_KEY);
        if (isRole(stored)) setSession(personaSession(stored));
      } else {
        const res = await api.GET("/api/v1/auth/session");
        const p = res.data?.principal;
        const role = p?.roles[0];
        if (!cancelled && res.data && p && role) {
          setCsrfToken(res.data.csrfToken);
          setSession({ role, displayName: p.displayName, loginId: p.loginId });
        }
      }
      if (!cancelled) setReady(true);
    })().catch(() => setReady(true));
    return () => {
      cancelled = true;
    };
  }, []);

  const signIn = useCallback(async (role: Role) => {
    if (AUTH_MODE === "mock") {
      window.sessionStorage.setItem(STORAGE_KEY, role);
      setSession(personaSession(role));
      return;
    }
    const res = unwrap(
      await api.POST("/api/v1/auth/login", {
        body: { loginId: ROLE_CONFIG[role].loginId, password: import.meta.env.VITE_DEV_PASSWORD ?? "sutra" },
      }),
    );
    if (res.outcome !== "authenticated" || !res.principal) {
      throw new Error("Multi-factor verification is required for this account (Batch 1 UI).");
    }
    const p = res.principal;
    setCsrfToken(res.csrfToken ?? undefined);
    setSession({ role: p.roles[0] ?? role, displayName: p.displayName, loginId: p.loginId });
  }, []);

  const signOut = useCallback(async () => {
    if (AUTH_MODE === "mock") window.sessionStorage.removeItem(STORAGE_KEY);
    // Logout is an unsafe request: the client middleware attaches X-CSRF-Token.
    else await api.POST("/api/v1/auth/logout").catch(() => undefined);
    setCsrfToken(undefined);
    setSession(null);
  }, []);

  const value = useMemo(() => ({ ready, session, signIn, signOut }), [ready, session, signIn, signOut]);
  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used inside <AuthProvider>");
  return ctx;
}

/** The signed-in session; only call inside the authenticated shell. */
export function useSession(): ClientSession {
  const { session } = useAuth();
  if (!session) throw new Error("No active session");
  return session;
}
