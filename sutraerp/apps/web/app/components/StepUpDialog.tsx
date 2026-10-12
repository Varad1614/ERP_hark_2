/**
 * Step-up verification dialog (passkey or authenticator code), shared by the Security page and
 * the Batch 2 admin screens: a command refused with `mfa-step-up-required` opens it, and the
 * caller retries the command after `onDone` adopted the rotated session.
 */
import { KeyRound } from "lucide-react";
import { ApiProblem, type LoginResponse, type MfaStatus } from "@sutra/api-client";
import { Button, Modal } from "@sutra/ui";
import { useEffect, useRef, useState } from "react";
import { api, unwrap } from "../lib/api";
import { getPasskeyAssertion, PasskeyCancelled, passkeyErrorMessage, webauthnSupported } from "../lib/webauthn";
import { TotpChallenge } from "./TotpChallenge";

export function problemMessage(e: unknown, ceremony: "create" | "get" = "create"): string {
  if (e instanceof ApiProblem) {
    if (e.problem.errorCode === "mfa-step-up-required") return "Verify it’s you first.";
    const violation = (e.problem as { validationErrors?: { message?: string }[] }).validationErrors?.[0]?.message;
    return violation ? `The passkey wasn’t accepted: ${violation}.` : (e.problem.detail ?? e.problem.title);
  }
  if (e instanceof PasskeyCancelled || e instanceof DOMException) return passkeyErrorMessage(e, ceremony);
  return e instanceof Error ? e.message : "Something went wrong";
}

/** One cancellable WebAuthn ceremony at a time; aborted on unmount. */
export function useCeremony() {
  const ref = useRef<AbortController | null>(null);
  const [running, setRunning] = useState(false);
  useEffect(() => () => ref.current?.abort(), []);
  const run = async <T,>(fn: (signal: AbortSignal) => Promise<T>): Promise<T> => {
    ref.current?.abort();
    const abort = new AbortController();
    ref.current = abort;
    setRunning(true);
    try {
      return await fn(abort.signal);
    } finally {
      if (ref.current === abort) {
        ref.current = null;
        setRunning(false);
      }
    }
  };
  return { running, run, cancel: () => ref.current?.abort() };
}

/** Step-up dialog: passkey or authenticator code; resolves with the rotated session. */
export function StepUp({
  status,
  open,
  onClose,
  onDone,
  message = "Changing your second factors needs a fresh verification.",
}: {
  status: MfaStatus;
  open: boolean;
  onClose: () => void;
  onDone: (res: LoginResponse) => void;
  message?: string;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ceremony = useCeremony();
  const viaPasskey = async () => {
    setBusy(true);
    setError(null);
    try {
      const res = await ceremony.run(async (signal) => {
        const c = unwrap(await api.POST("/api/v1/auth/mfa/webauthn/assertion"));
        const credential = await getPasskeyAssertion(c.options, signal);
        return unwrap(await api.POST("/api/v1/auth/mfa/webauthn/step-up", { body: { ceremonyId: c.ceremonyId, credential } }));
      });
      onDone(res);
    } catch (e) {
      setError(e instanceof ApiProblem && e.problem.status === 401 ? "That passkey wasn’t accepted. Try again." : problemMessage(e, "get"));
    } finally {
      setBusy(false);
    }
  };
  const close = () => {
    ceremony.cancel();
    setError(null);
    onClose();
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
    <Modal open={open} onClose={close} label="Verify it’s you">
      <h2>Verify it’s you</h2>
      <p>{message}</p>
      {status.passkeys.length > 0 && (
        <div className="sb-actions-row">
          <Button icon={<KeyRound strokeWidth={1.75} />} iconPosition="start" onClick={viaPasskey} disabled={busy || !webauthnSupported()} data-stepup-passkey>
            {ceremony.running ? "Waiting for your passkey…" : "Use a passkey"}
          </Button>
          {ceremony.running && (
            <Button variant="ghost" onClick={ceremony.cancel} data-stepup-cancel>
              Cancel
            </Button>
          )}
        </div>
      )}
      {status.totpEnrolled && <TotpChallenge title="Authenticator code" submitLabel="Verify" busy={busy} error={error} onSubmit={viaCode} />}
      {!status.totpEnrolled && error && (
        <p className="sb-field-error" role="alert" data-stepup-error>
          {error}
        </p>
      )}
      {!status.totpEnrolled && !ceremony.running && (
        <div className="sb-actions-row">
          <Button variant="ghost" onClick={close}>
            Close
          </Button>
        </div>
      )}
    </Modal>
  );
}
