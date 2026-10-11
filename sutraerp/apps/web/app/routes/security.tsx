import { KeyRound, LaptopMinimal, LogOut, ShieldCheck, Smartphone, Trash2 } from "lucide-react";
import { ApiProblem, type LoginResponse, type MfaStatus, type SessionView } from "@sutra/api-client";
import { Badge, BentoGrid, BentoTile, Button, EmptyState, IconChip, Input, Modal, useToast } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { TotpChallenge } from "../components/TotpChallenge";
import { api, AUTH_MODE, unwrap, USES_API } from "../lib/api";
import { useAuth, useSession } from "../lib/auth";
import { inboxKeys, relativeTime, useInbox, useInboxRefresh } from "../lib/inbox";
import { createPasskey, getPasskeyAssertion, webauthnSupported } from "../lib/webauthn";

export function meta() {
  return [{ title: "Security · SutraERP" }];
}

/** Human label for a session's authentication methods. */
export function describeSession(s: Pick<SessionView, "amr" | "federated" | "mfaLevel">): string {
  const how = s.federated ? "Single sign-on" : "Password";
  const second = s.amr.includes("hwk") ? " + passkey" : s.amr.includes("otp") ? " + authenticator" : s.mfaLevel === "mfa" ? " + second factor" : "";
  return how + second;
}

function problemMessage(e: unknown): string {
  if (e instanceof ApiProblem) {
    if (e.problem.errorCode === "mfa-step-up-required") return "Verify it’s you first.";
    return e.problem.detail ?? e.problem.title;
  }
  if (e instanceof DOMException && e.name === "NotAllowedError") return "The passkey prompt was cancelled.";
  return e instanceof Error ? e.message : "Something went wrong";
}

/** Step-up dialog: passkey or authenticator code; resolves with the rotated session. */
function StepUp({ status, open, onClose, onDone }: { status: MfaStatus; open: boolean; onClose: () => void; onDone: (res: LoginResponse) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const viaPasskey = async () => {
    setBusy(true);
    setError(null);
    try {
      const c = unwrap(await api.POST("/api/v1/auth/mfa/webauthn/assertion"));
      const credential = await getPasskeyAssertion(c.options);
      onDone(unwrap(await api.POST("/api/v1/auth/mfa/webauthn/step-up", { body: { ceremonyId: c.ceremonyId, credential } })));
    } catch (e) {
      setError(problemMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const viaCode = async (code: string) => {
    setBusy(true);
    setError(null);
    try {
      onDone(unwrap(await api.POST("/api/v1/auth/mfa/step-up", { body: { code } })));
    } catch (e) {
      setError(e instanceof ApiProblem && e.problem.status === 401 ? "That code didn’t work — wait for the next one." : problemMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal open={open} onClose={onClose} label="Verify it’s you">
      <h2>Verify it’s you</h2>
      <p>Changing your second factors needs a fresh verification.</p>
      {status.passkeys.length > 0 && (
        <div className="sb-actions-row">
          <Button icon={<KeyRound strokeWidth={1.75} />} iconPosition="start" onClick={viaPasskey} disabled={busy || !webauthnSupported()} data-stepup-passkey>
            Use a passkey
          </Button>
        </div>
      )}
      {status.totpEnrolled && <TotpChallenge title="Authenticator code" submitLabel="Verify" busy={busy} error={error} onSubmit={viaCode} />}
      {!status.totpEnrolled && error && <p className="sb-field-error">{error}</p>}
    </Modal>
  );
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
  const [busy, setBusy] = useState(false);
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

  const registerPasskey = () =>
    guarded(async () => {
      setBusy(true);
      try {
        const c = unwrap(await api.POST("/api/v1/auth/mfa/webauthn/registration"));
        const credential = await createPasskey(c.options);
        adoptSession(
          unwrap(await api.POST("/api/v1/auth/mfa/webauthn/registration/complete", { body: { ceremonyId: c.ceremonyId, credential, label: label.trim() } })),
        );
        setLabel("");
        toast("Passkey added");
        await refresh();
      } finally {
        setBusy(false);
      }
    });

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
              <article key={s.id} className="sb-list-row" data-session={s.id}>
                <IconChip icon={LaptopMinimal} tone={s.current ? "green" : "grey"} size="sm" />
                <div>
                  <h3>
                    {describeSession(s)}
                    {s.current && <Badge kind="done">This device</Badge>}
                    {s.mfaLevel === "mfa" && <Badge kind="info">Verified</Badge>}
                  </h3>
                  <small>
                    Signed in {relativeTime(s.createdAt)} · active {relativeTime(s.lastSeenAt)}
                  </small>
                </div>
                {!s.current && (
                  <div className="sb-row-actions">
                    <Button size="sm" variant="ghost" onClick={() => revoke(s)} data-revoke={s.id}>
                      Sign out
                    </Button>
                  </div>
                )}
              </article>
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
              <article className="sb-list-row" data-totp-status>
                <IconChip icon={Smartphone} tone="violet" size="sm" />
                <div>
                  <h3>
                    Authenticator app {status.totpEnrolled ? <Badge kind="done">On</Badge> : <Badge kind="neutral">Off</Badge>}
                  </h3>
                  <small>Six-digit codes from Google Authenticator, Microsoft Authenticator, 1Password…</small>
                </div>
                <div className="sb-row-actions">
                  {status.totpEnrolled ? (
                    <Button size="sm" variant="ghost" onClick={disableTotp} data-disable-totp disabled={!USES_API}>
                      Turn off
                    </Button>
                  ) : (
                    <Button size="sm" onClick={() => navigate("/account/mfa")} disabled={!USES_API}>
                      Set up
                    </Button>
                  )}
                </div>
              </article>
              {status.passkeys.map((p) => (
                <article key={p.id} className="sb-list-row" data-passkey={p.id}>
                  <IconChip icon={KeyRound} tone="orange" size="sm" />
                  <div>
                    <h3>{p.label}</h3>
                    <small>
                      Added {relativeTime(p.createdAt)}
                      {p.lastUsedAt ? ` · used ${relativeTime(p.lastUsedAt)}` : " · not used yet"}
                    </small>
                  </div>
                  <div className="sb-row-actions">
                    <Button size="sm" variant="ghost" icon={<Trash2 strokeWidth={1.75} />} onClick={() => removePasskey(p.id)} aria-label={`Remove ${p.label}`}>
                      Remove
                    </Button>
                  </div>
                </article>
              ))}
            </div>
            {status.webauthnAvailable ? (
              <div className="sb-form" data-passkey-form>
                <Input label="Passkey name" name="label" placeholder="Work laptop" maxLength={64} value={label} onChange={(e) => setLabel(e.target.value)} />
                <div className="sb-actions-row">
                  <Button icon={<KeyRound strokeWidth={1.75} />} iconPosition="start" onClick={registerPasskey} disabled={busy || !webauthnSupported()} data-add-passkey>
                    {busy ? "Waiting for your device…" : "Add a passkey"}
                  </Button>
                </div>
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
