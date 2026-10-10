import { KeyRound, ShieldCheck, Smartphone } from "lucide-react";
import { ApiProblem, type TotpEnrolmentResponse } from "@sutra/api-client";
import { BentoGrid, BentoTile, Button, IconChip, useToast } from "@sutra/ui";
import { useState } from "react";
import { useNavigate } from "react-router";
import { QrCode } from "../components/QrCode";
import { TotpChallenge } from "../components/TotpChallenge";
import { AUTH_MODE, api, unwrap } from "../lib/api";
import { useAuth, useSession } from "../lib/auth";

export function meta() {
  return [{ title: "Two-step verification · SutraERP" }];
}

function message(e: unknown): string {
  if (e instanceof ApiProblem) {
    if (e.problem.errorCode === "mfa-step-up-required") return "Verify with your current authenticator before replacing it.";
    if (e.problem.errorCode === "validation-failed") return "That code doesn’t match — check the app and try again.";
  }
  return e instanceof Error ? e.message : "Something went wrong";
}

/** TOTP enrolment: QR (or secret) from the `otpauth://` URI, then confirm the first code. */
export default function MfaEnrol() {
  const session = useSession();
  const { adoptSession } = useAuth();
  const toast = useToast();
  const navigate = useNavigate();
  const [enrolment, setEnrolment] = useState<TotpEnrolmentResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      setEnrolment(unwrap(await api.POST("/api/v1/auth/mfa/totp/enrolment")));
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  const confirm = async (code: string) => {
    setBusy(true);
    setError(null);
    try {
      adoptSession(unwrap(await api.POST("/api/v1/auth/mfa/totp/confirm", { body: { code } })));
      toast("Two-step verification is on");
      navigate("/", { replace: true });
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  if (AUTH_MODE !== "api") {
    return (
      <BentoGrid data-screen="mfa">
        <BentoTile variant="accent" className="sb-soon">
          <span className="sb-chip">
            <ShieldCheck strokeWidth={1.75} /> Two-step verification
          </span>
          <h1>{AUTH_MODE === "oidc" ? "Managed by single sign-on" : "Available with the live API"}</h1>
          <p>
            {AUTH_MODE === "oidc"
              ? "Authenticator apps and security keys are set up in the SutraERP sign-in (Keycloak) account console."
              : "Run the web app with VITE_AUTH_MODE=api to enrol an authenticator against sutra-api."}
          </p>
        </BentoTile>
      </BentoGrid>
    );
  }

  return (
    <BentoGrid data-screen="mfa">
      <BentoTile variant="accent" className="sb-soon sb-span-7">
        <span className="sb-chip">
          <ShieldCheck strokeWidth={1.75} /> Two-step verification
        </span>
        <h1>{session.mfaLevel === "mfa" ? "Your session is verified" : "Protect your account"}</h1>
        <p>
          {session.mfaEnrollmentRequired
            ? "Your role requires an authenticator app. Admin-sensitive actions stay locked until you finish this step."
            : "Add an authenticator app (Google Authenticator, Microsoft Authenticator, 1Password …) for a second sign-in factor."}
        </p>
        {!enrolment && (
          <div className="sb-actions-row">
            <Button icon={<Smartphone strokeWidth={1.75} />} onClick={start} disabled={busy} data-mfa-start>
              {busy ? "Preparing…" : "Set up authenticator"}
            </Button>
          </div>
        )}
        {!enrolment && error && <p className="sb-field-error">{error}</p>}
      </BentoTile>
      {enrolment && (
        <>
          <BentoTile className="sb-span-5 sb-mfa-qr" title="1 · Scan">
            <QrCode value={enrolment.otpauthUri} label="Authenticator QR code" />
            <p className="sb-mfa-secret">
              <IconChip icon={KeyRound} tone="grey" size="sm" />
              <span>
                Can’t scan? Enter <code data-mfa-secret>{enrolment.secret.replace(/(.{4})/g, "$1 ").trim()}</code>
              </span>
            </p>
          </BentoTile>
          <div className="sb-span-7">
            <TotpChallenge
              title="2 · Confirm"
              description={`Enter the ${enrolment.digits}-digit code your app shows now.`}
              submitLabel="Turn on"
              busy={busy}
              error={error}
              onSubmit={confirm}
            />
          </div>
        </>
      )}
    </BentoGrid>
  );
}
