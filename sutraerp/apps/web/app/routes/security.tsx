import { ArrowLeft, KeyRound, LaptopMinimal, LogOut, ShieldCheck, Smartphone, Trash2 } from "lucide-react";
import { ApiProblem, type SessionView } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, EmptyState, ListRow, TextField, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { problemMessage, StepUp, useCeremony } from "../components/StepUpDialog";
import { api, AUTH_MODE, unwrap, USES_API } from "../lib/api";
import { useAuth, useSession } from "../lib/auth";
import { inboxKeys, relativeTime, useInbox, useInboxRefresh } from "../lib/inbox";
import { createPasskey, webauthnSupported } from "../lib/webauthn";

export function meta() {
  return [{ title: "Security · SutraERP" }];
}

/** Human label for a session's authentication methods. */
export function describeSession(s: Pick<SessionView, "amr" | "federated" | "mfaLevel">): string {
  const how = s.federated ? "Single sign-on" : "Password";
  const second = s.amr.includes("hwk") ? " + passkey" : s.amr.includes("otp") ? " + authenticator" : s.mfaLevel === "mfa" ? " + second factor" : "";
  return how + second;
}

/** Security & sign-in: active sessions, second factors (authenticator app, passkeys). */
export default function Security() {
  const session = useSession();
  const { adoptSession, signOut } = useAuth();
  const inbox = useInbox();
  const refresh = useInboxRefresh();
  const toast = useToast();
  const navigate = useNavigate();
  const sessions = useQuery({ queryKey: inboxKeys.sessions(session.role), queryFn: () => inbox.sessions() });
  const mfa = useQuery({ queryKey: inboxKeys.mfa(session.role), queryFn: () => inbox.mfa() });
  const [label, setLabel] = useState("");
  const [passkeyError, setPasskeyError] = useState<string | null>(null);
  const enrol = useCeremony();
  const [stepUp, setStepUp] = useState<null | (() => Promise<void>)>(null);

  /** Runs `action`; on a step-up problem opens the verification dialog and retries after it. */
  const guarded = async (action: () => Promise<void>) => {
    try {
      await action();
    } catch (e) {
      if (e instanceof ApiProblem && e.problem.errorCode === "mfa-step-up-required") setStepUp(() => action);
      else toast(problemMessage(e));
    }
  };

  /**
   * Registration: options → browser/OS prompt → verify + store. The prompt can be cancelled from
   * the page (the old UI stayed on "Waiting for your device…" until the 5-minute timeout); every
   * failure ends in a message next to the button.
   */
  const registerPasskey = async () => {
    setPasskeyError(null);
    const attempt = async () => {
      const res = await enrol.run(async (signal) => {
        const c = unwrap(await api.POST("/api/v1/auth/mfa/webauthn/registration"));
        const credential = await createPasskey(c.options, signal);
        return unwrap(
          await api.POST("/api/v1/auth/mfa/webauthn/registration/complete", { body: { ceremonyId: c.ceremonyId, credential, label: label.trim() } }),
        );
      });
      adoptSession(res);
      setLabel("");
      toast("Passkey added");
      await refresh();
    };
    try {
      await attempt();
    } catch (e) {
      if (e instanceof ApiProblem && e.problem.errorCode === "mfa-step-up-required") setStepUp(() => attempt);
      else setPasskeyError(problemMessage(e, "create"));
    }
  };

  const removePasskey = (id: string) =>
    guarded(async () => {
      const r = await api.DELETE("/api/v1/auth/mfa/webauthn/credentials/{id}", { params: { path: { id } } });
      if (r.error !== undefined) unwrap(r);
      toast("Passkey removed");
      await refresh();
    });

  const disableTotp = () =>
    guarded(async () => {
      const r = await api.DELETE("/api/v1/auth/mfa/totp");
      if (r.error !== undefined) unwrap(r);
      toast("Authenticator app removed");
      await refresh();
    });

  const revoke = async (s: SessionView) => {
    try {
      await inbox.revokeSession(s.id);
      toast("Signed out that session");
      await refresh();
    } catch (e) {
      toast(problemMessage(e));
    }
  };
  const revokeOthers = async () => {
    try {
      const n = await inbox.revokeOthers();
      toast(n === 1 ? "Signed out 1 other session" : `Signed out ${n} other sessions`);
      await refresh();
    } catch (e) {
      toast(problemMessage(e));
    }
  };

  const status = mfa.data;
  const others = (sessions.data ?? []).filter((s) => !s.current).length;

  return (
    <BentoGrid data-screen="security">
      <BentoTile span={12} className="sb-area-head" data-area-head="">
        <div className="sb-area-title">
          <button type="button" className="sb-back sb-page-eyebrow" onClick={() => navigate("/profile")} data-back-profile>
            <ArrowLeft strokeWidth={1.75} /> My profile
          </button>
          <h1>Security & sign-in</h1>
          <p className="sb-area-summary" data-area-summary="">
            {sessions.data ? `${sessions.data.length} active session${sessions.data.length === 1 ? "" : "s"}` : "Sessions"}
            {status ? ` · ${status.totpEnrolled || status.passkeys.length ? "two-step verification on" : "password only"}` : ""}
          </p>
        </div>
      </BentoTile>
      <BentoTile span={7} title="Where you’re signed in" action={
        <Button size="sm" variant="ghost" icon={<LogOut strokeWidth={1.75} />} onClick={revokeOthers} disabled={others === 0} data-revoke-others>
          Sign out others
        </Button>
      }>
        {sessions.isError ? (
          <p className="sb-field-error">{problemMessage(sessions.error)}</p>
        ) : (
          <div className="sb-list" data-session-list>
            {(sessions.data ?? []).map((s) => (
              <ListRow
                key={s.id}
                icon={LaptopMinimal}
                tone={s.current ? "green" : "grey"}
                title={
                  <>
                    {describeSession(s)} {s.current && <Badge kind="done">This device</Badge>} {s.mfaLevel === "mfa" && <Badge kind="info">Verified</Badge>}
                  </>
                }
                meta={`Signed in ${relativeTime(s.createdAt)} · active ${relativeTime(s.lastSeenAt)}`}
                primary={s.current ? undefined : { label: "Sign out", ghost: true, onClick: () => revoke(s), "data-revoke": s.id }}
                data-session={s.id}
              />
            ))}
          </div>
        )}
        <div className="sb-actions-row">
          <Button variant="ghost" onClick={async () => { await signOut(); navigate("/sign-in", { replace: true }); }}>
            Sign out of this device
          </Button>
        </div>
      </BentoTile>

      <BentoTile span={5} title="Two-step verification">
        {!status ? (
          <div className="sb-skeleton" />
        ) : status.managedBy === "keycloak" || AUTH_MODE === "oidc" ? (
          <EmptyState icon={ShieldCheck} title="Managed by single sign-on">
            Authenticator apps and passkeys are set up in the SutraERP sign-in (Keycloak) account console.
          </EmptyState>
        ) : (
          <>
            <p>
              This session is {status.sessionMfaLevel === "mfa" ? "verified with a second factor" : "signed in with a password only"}.
              {status.mfaRequired ? " Your role requires a second factor." : ""}
            </p>
            <div className="sb-list">
              <ListRow
                icon={Smartphone}
                tone="violet"
                title={<>Authenticator app {status.totpEnrolled ? <Badge kind="done">On</Badge> : <Badge kind="neutral">Off</Badge>}</>}
                meta="Six-digit codes from Google Authenticator, Microsoft Authenticator, 1Password…"
                primary={
                  status.totpEnrolled
                    ? { label: "Turn off", ghost: true, onClick: disableTotp, disabled: !USES_API, "data-disable-totp": "" }
                    : { label: "Set up", onClick: () => navigate("/account/mfa"), disabled: !USES_API, "data-setup-totp": "" }
                }
                data-totp-status=""
              />
              {status.passkeys.map((p) => (
                <ListRow
                  key={p.id}
                  icon={KeyRound}
                  tone="orange"
                  title={p.label}
                  meta={`Added ${relativeTime(p.createdAt)}${p.lastUsedAt ? ` · used ${relativeTime(p.lastUsedAt)}` : " · not used yet"}`}
                  primary={{ label: "Remove", ghost: true, icon: Trash2, onClick: () => removePasskey(p.id), "data-remove-passkey": p.id }}
                  data-passkey={p.id}
                />
              ))}
            </div>
            {status.webauthnAvailable ? (
              <div className="sb-form" data-passkey-form>
                <TextField label="Passkey name" name="label" placeholder="Work laptop" maxLength={64} value={label} onChange={(e) => setLabel(e.target.value)} />
                <div className="sb-actions-row">
                  <Button icon={<KeyRound strokeWidth={1.75} />} iconPosition="start" onClick={registerPasskey} disabled={enrol.running || !webauthnSupported()} data-add-passkey>
                    {enrol.running ? "Waiting for your device…" : "Add a passkey"}
                  </Button>
                  {enrol.running && (
                    <Button variant="ghost" onClick={enrol.cancel} data-passkey-cancel>
                      Cancel
                    </Button>
                  )}
                </div>
                {enrol.running && (
                  <p>
                    <small>Follow your browser’s prompt. On Windows choose Windows Hello (PIN, face or fingerprint), a phone, or a security key.</small>
                  </p>
                )}
                {passkeyError && (
                  <p className="sb-field-error" role="alert" data-passkey-error>
                    {passkeyError}
                  </p>
                )}
                {!webauthnSupported() && <p className="sb-field-error">This browser cannot use passkeys here (https or localhost is required).</p>}
              </div>
            ) : (
              <p><small>Passkeys are available with the live API in development sign-in.</small></p>
            )}
          </>
        )}
      </BentoTile>
      {status && (
        <StepUp
          status={status}
          open={stepUp !== null}
          onClose={() => setStepUp(null)}
          onDone={async (res) => {
            adoptSession(res);
            const retry = stepUp;
            setStepUp(null);
            toast("Verified");
            await refresh();
            if (retry) await guarded(retry);
          }}
        />
      )}
    </BentoGrid>
  );
}
