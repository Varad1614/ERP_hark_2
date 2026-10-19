import { ArrowLeft, ShieldCheck } from "lucide-react";
import { BentoTile, Button, IconChip, Input } from "@sutra/ui";
import { useState, type FormEvent } from "react";

/** Six ASCII digits — the only shape a TOTP code can take. */
export function isTotpCode(code: string): boolean {
  return /^\d{6}$/.test(code);
}

export interface TotpChallengeProps {
  title?: string;
  description?: string;
  submitLabel?: string;
  busy?: boolean;
  error?: string | null;
  onSubmit: (code: string) => void | Promise<void>;
  onCancel?: () => void;
}

/** Soft Bento TOTP prompt used by the sign-in challenge, enrolment confirmation and step-up. */
export function TotpChallenge({
  title = "Two-step verification",
  description = "Enter the 6-digit code from your authenticator app.",
  submitLabel = "Verify",
  busy = false,
  error,
  onSubmit,
  onCancel,
}: TotpChallengeProps) {
  const [code, setCode] = useState("");
  const valid = isTotpCode(code);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (valid && !busy) void onSubmit(code);
  };
  return (
    <BentoTile className="sb-mfa" data-mfa-challenge>
      <form onSubmit={submit} noValidate>
        <IconChip icon={ShieldCheck} tone="violet" size="md" />
        <h2>{title}</h2>
        <p>{description}</p>
        <Input
          label="Authentication code"
          name="code"
          inputMode="numeric"
          autoComplete="one-time-code"
          pattern="[0-9]{6}"
          maxLength={6}
          autoFocus
          value={code}
          error={error ?? undefined}
          onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
        />
        <div className="sb-actions-row">
          <Button type="submit" disabled={!valid || busy}>
            {busy ? "Verifying…" : submitLabel}
          </Button>
          {onCancel && (
            <Button variant="ghost" icon={<ArrowLeft strokeWidth={1.75} />} iconPosition="start" onClick={onCancel} disabled={busy}>
              Back
            </Button>
          )}
        </div>
      </form>
    </BentoTile>
  );
}
