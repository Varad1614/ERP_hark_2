import { ArrowUpRight, KeyRound } from "lucide-react";
import { ApiProblem, type MfaMethod } from "@sutra/api-client";
import { Button, Logo, ROLE_ACCENTS, RoleTheme, useToast } from "@sutra/ui";
import type { CSSProperties } from "react";
import { useEffect, useRef, useState } from "react";
import { Navigate, useNavigate, useSearchParams } from "react-router";
import { TotpChallenge } from "../components/TotpChallenge";
import { AUTH_MODE, oidcStartUrl } from "../lib/api";
import { useAuth, type ClientSession } from "../lib/auth";
import { ROLE_CONFIG, ROLES, type Role } from "../lib/roles";
import { PasskeyCancelled, passkeyErrorMessage, webauthnSupported } from "../lib/webauthn";

export function meta() {
  return [{ title: "Sign in · SutraERP" }];
}

/** Messages for `/sign-in?authError=…` set by the OIDC callback. */
const AUTH_ERRORS: Record<string, string> = {
  "state-invalid": "That sign-in link expired or was already used. Please start again.",
  "browser-mismatch": "Sign-in must finish in the browser that started it. Please start again.",
  "token-invalid": "The identity provider’s response could not be verified.",
  "access-denied": "Your account has no SutraERP access for this institution.",
  unavailable: "The sign-in service is unavailable right now. Try again shortly.",
};

function describe(e: unknown): string {
  if (e instanceof ApiProblem && e.problem.status === 401) return "That didn’t work. Sign in again to get a new challenge.";
  if (e instanceof ApiProblem) return e.message;
  if (e instanceof PasskeyCancelled || e instanceof DOMException) return passkeyErrorMessage(e, "get");
  return e instanceof Error ? e.message : "Sign-in failed";
}

export default function SignIn() {
  const { ready, session, signIn, verifyMfa, verifyPasskey } = useAuth();
  const navigate = useNavigate();
  const toast = useToast();
  const [params] = useSearchParams();
  const [busy, setBusy] = useState<Role | null>(null);
  const [challenge, setChallenge] = useState<{ role: Role; id: string; methods: MfaMethod[] } | null>(null);
  const [mfaBusy, setMfaBusy] = useState(false);
  const [mfaError, setMfaError] = useState<string | null>(null);
  const [passkeyBusy, setPasskeyBusy] = useState(false);
  const passkeyAbort = useRef<AbortController | null>(null);
  useEffect(() => () => passkeyAbort.current?.abort(), []);
  if (ready && session && !challenge) return <Navigate to="/" replace />;

  const finish = (s: ClientSession) => {
    toast(`Signed in as ${s.displayName}`);
    navigate(s.mfaEnrollmentRequired ? "/account/mfa" : "/", { replace: true });
  };

  const choose = async (role: Role) => {
    setBusy(role);
    try {
      const result = await signIn(role);
      if (result.status === "mfaRequired") {
        setMfaError(null);
        setChallenge({ role, id: result.challengeId, methods: result.methods });
      } else finish(result.session);
    } catch (e) {
      toast(e instanceof Error ? e.message : "Sign-in failed");
    } finally {
      setBusy(null);
    }
  };

  const verify = async (code: string) => {
    if (!challenge) return;
    setMfaBusy(true);
    try {
      const s = await verifyMfa(challenge.id, code);
      setChallenge(null);
      finish(s);
    } catch (e) {
      // Challenges are single use: a failed attempt needs a fresh sign-in.
      setMfaError(describe(e));
    } finally {
      setMfaBusy(false);
    }
  };

  const passkey = async () => {
    if (!challenge) return;
    const abort = new AbortController();
    passkeyAbort.current = abort;
    setMfaBusy(true);
    setPasskeyBusy(true);
    setMfaError(null);
    try {
      const s = await verifyPasskey(challenge.id, (id) => setChallenge((c) => (c ? { ...c, id } : c)), abort.signal);
      setChallenge(null);
      finish(s);
    } catch (e) {
      // The renewed challenge still accepts the authenticator code after a cancelled prompt.
      setMfaError(describe(e) + (e instanceof ApiProblem ? "" : challenge.methods.includes("totp") ? " You can try again or use your authenticator code." : " You can try again."));
    } finally {
      if (passkeyAbort.current === abort) passkeyAbort.current = null;
      setMfaBusy(false);
      setPasskeyBusy(false);
    }
  };

  const authError = params.get("authError");
  const offersPasskey = challenge?.methods.includes("webauthn") ?? false;
  const offersTotp = challenge ? challenge.methods.length === 0 || challenge.methods.includes("totp") : false;

  return (
    <div className="sb-signin">
      <RoleTheme role={challenge?.role ?? null} />
      <Logo />
      {challenge ? (
        <>
          <h1>Verify it’s you</h1>
          <p>{ROLE_CONFIG[challenge.role].name} · second factor required</p>
          <div className="sb-mfa-wrap">
            {offersPasskey && (
              <div className="sb-actions-row sb-signin-passkey">
                <Button
                  icon={<KeyRound strokeWidth={1.75} />}
                  iconPosition="start"
                  onClick={passkey}
                  disabled={mfaBusy || !webauthnSupported()}
                  data-passkey-signin
                >
                  {passkeyBusy ? "Waiting for your passkey…" : "Use a passkey"}
                </Button>
                {passkeyBusy && (
                  <Button variant="ghost" onClick={() => passkeyAbort.current?.abort()} data-passkey-cancel>
                    Cancel
                  </Button>
                )}
                {!webauthnSupported() && <small>This browser cannot use passkeys here.</small>}
              </div>
            )}
            {offersTotp ? (
              <TotpChallenge
                description={offersPasskey ? "Or enter the 6-digit code from your authenticator app." : undefined}
                busy={mfaBusy}
                error={mfaError}
                onSubmit={verify}
                onCancel={() => {
                  setChallenge(null);
                  setMfaError(null);
                }}
              />
            ) : (
              <>
                {mfaError && (
                  <p className="sb-field-error" role="alert" data-mfa-error>
                    {mfaError}
                  </p>
                )}
                <div className="sb-actions-row">
                  <Button
                    variant="ghost"
                    onClick={() => {
                      setChallenge(null);
                      setMfaError(null);
                    }}
                  >
                    Back
                  </Button>
                </div>
              </>
            )}
          </div>
        </>
      ) : (
        <>
          <h1>Sign in to SutraERP</h1>
          {authError && (
            <p className="sb-field-error" role="alert" data-auth-error={authError}>
              {AUTH_ERRORS[authError] ?? "Sign-in failed. Please try again."}
            </p>
          )}
          {AUTH_MODE === "oidc" ? (
            <>
              <p>Sutra Institute of Technology · single sign-on</p>
              <div className="sb-actions-row sb-signin-sso">
                <Button
                  icon={<KeyRound strokeWidth={1.75} />}
                  iconPosition="start"
                  onClick={() => window.location.assign(oidcStartUrl("/"))}
                  data-oidc-start
                >
                  Continue with SutraERP SSO
                </Button>
              </div>
            </>
          ) : (
            <>
              <p>Sutra Institute of Technology · development sign-in — choose a role</p>
              <div className="sb-role-cards">
                {ROLES.map((role) => {
                  const r = ROLE_CONFIG[role];
                  return (
                    <button
                      key={role}
                      type="button"
                      className="sb-tile sb-role-card"
                      data-role-card={role}
                      disabled={busy !== null}
                      onClick={() => choose(role)}
                      style={{ "--ac": ROLE_ACCENTS[role].ac, "--acs": ROLE_ACCENTS[role].acs, "--act": ROLE_ACCENTS[role].act } as CSSProperties}
                    >
                      <span className="sb-avatar">{r.initials}</span>
                      <small>{r.title}</small>
                      <h3>{r.name}</h3>
                      <small>{r.blurb}</small>
                      <span className="sb-btn">
                        {busy === role ? "Signing in…" : `Continue as ${r.title.toLowerCase()}`} <ArrowUpRight strokeWidth={1.75} />
                      </span>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </>
      )}
      <div className="sb-env-tag">
        <i />
        {AUTH_MODE === "oidc" ? "Keycloak · OIDC" : AUTH_MODE === "api" ? "Dev authenticator · API" : "Dev personas · mock data"}
      </div>
    </div>
  );
}
