/**
 * Client session state. The server-side session cookie is HttpOnly (ADR-L5-0038), so in `api` /
 * `oidc` mode the browser only ever learns the principal (and the session's CSRF token) through
 * the login / MFA responses and `GET /api/v1/auth/session`.
 *
 * Sign-in in `api` mode: `GET /auth/csrf` (login-CSRF bootstrap) → `POST /auth/login` → either a
 * session, or `mfaRequired` + `challengeId` → `POST /auth/mfa/verify` with a TOTP code (same
 * pre-session token). In `oidc` mode the sign-in page links to `GET /auth/oidc/start` instead.
 * In `mock` mode (default for UI work) the chosen dev persona is kept in sessionStorage.
 */
import type { LoginResponse, MfaLevel } from "@sutra/api-client";
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { AUTH_MODE, api, bootstrapLoginCsrf, setCsrfToken, unwrap } from "./api";
import { isRole, ROLE_CONFIG, type Role } from "./roles";

export interface ClientSession {
  role: Role;
  displayName: string;
  loginId: string;
  /** `mfa` once a second factor was verified for this session. */
  mfaLevel: MfaLevel;
  /** The account must enrol an authenticator before admin-sensitive actions. */
  mfaEnrollmentRequired: boolean;
}

export type SignInResult = { status: "authenticated"; session: ClientSession } | { status: "mfaRequired"; challengeId: string };

interface AuthContextValue {
  ready: boolean;
  session: ClientSession | null;
  signIn: (role: Role) => Promise<SignInResult>;
  verifyMfa: (challengeId: string, code: string) => Promise<ClientSession>;
  /** Adopts the response of an endpoint that rotated the session (MFA enrolment, step-up). */
  adoptSession: (res: LoginResponse) => ClientSession;
  signOut: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue | null>(null);
const STORAGE_KEY = "sutra.devRole";

function personaSession(role: Role): ClientSession {
  const c = ROLE_CONFIG[role];
  return { role, displayName: c.name, loginId: c.loginId, mfaLevel: "none", mfaEnrollmentRequired: false };
}

/** Maps an authenticated login/MFA response onto the client session (and stores its CSRF token). */
export function sessionFromLogin(res: LoginResponse, fallbackRole: Role): ClientSession {
  const p = res.principal;
  if (res.outcome !== "authenticated" || !p) throw new Error("Sign-in is not complete");
  setCsrfToken(res.csrfToken ?? undefined);
  return {
    role: p.roles[0] ?? fallbackRole,
    displayName: p.displayName,
    loginId: p.loginId,
    mfaLevel: res.mfaLevel ?? "none",
    mfaEnrollmentRequired: res.mfaEnrollmentRequired,
  };
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
          setSession({
            role,
            displayName: p.displayName,
            loginId: p.loginId,
            mfaLevel: res.data.mfaLevel,
            mfaEnrollmentRequired: res.data.mfaEnrollmentRequired,
          });
        }
      }
      if (!cancelled) setReady(true);
    })().catch(() => setReady(true));
    return () => {
      cancelled = true;
    };
  }, []);

  const signIn = useCallback(async (role: Role): Promise<SignInResult> => {
    if (AUTH_MODE === "mock") {
      window.sessionStorage.setItem(STORAGE_KEY, role);
      const s = personaSession(role);
      setSession(s);
      return { status: "authenticated", session: s };
    }
    if (AUTH_MODE === "oidc") throw new Error("Use single sign-on to continue.");
    // Login CSRF: the pre-session token is sent on login and on the MFA step that follows.
    await bootstrapLoginCsrf();
    const res = unwrap(
      await api.POST("/api/v1/auth/login", {
        body: { loginId: ROLE_CONFIG[role].loginId, password: import.meta.env.VITE_DEV_PASSWORD ?? "sutra" },
      }),
    );
    if (res.outcome === "mfaRequired" && res.challengeId) return { status: "mfaRequired", challengeId: res.challengeId };
    const s = sessionFromLogin(res, role);
    setSession(s);
    return { status: "authenticated", session: s };
  }, []);

  const verifyMfa = useCallback(async (challengeId: string, code: string) => {
    const res = unwrap(await api.POST("/api/v1/auth/mfa/verify", { body: { challengeId, code } }));
    const s = sessionFromLogin(res, "administrator");
    setSession(s);
    return s;
  }, []);

  const adoptSession = useCallback((res: LoginResponse) => {
    const s = sessionFromLogin(res, "administrator");
    setSession(s);
    return s;
  }, []);

  const signOut = useCallback(async () => {
    let endSessionUrl: string | undefined;
    if (AUTH_MODE === "mock") window.sessionStorage.removeItem(STORAGE_KEY);
    else {
      // Logout is an unsafe request: the client middleware attaches X-CSRF-Token.
      const res = await api.POST("/api/v1/auth/logout").catch(() => undefined);
      endSessionUrl = res?.data?.endSessionUrl;
    }
    setCsrfToken(undefined);
    setSession(null);
    // Keycloak session: finish single sign-out at the IdP (RP-initiated logout).
    if (endSessionUrl) window.location.assign(endSessionUrl);
  }, []);

  const value = useMemo(
    () => ({ ready, session, signIn, verifyMfa, adoptSession, signOut }),
    [ready, session, signIn, verifyMfa, adoptSession, signOut],
  );
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
