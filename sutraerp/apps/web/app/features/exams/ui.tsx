/**
 * Re-skin v2 (R4) building blocks of the Examinations screens, local to `features/exams` so the
 * shared Batch 2 helpers (`institution/forms`) stay untouched for the other areas:
 *
 * - `ExamActions` — one primary pill + kebab menu (`RowActions`) instead of clusters of text
 *   buttons. `data-action` stays on the pill; menu entries carry `data-menu-item` with the same id.
 * - `useExamAsk` — the command / create dialog in the prototype `StepDialog` style with the styled
 *   form controls (`FormSelect`, `DatePicker`, `TextField`, `Checkbox`). Same `AskSpec` as
 *   `institution/forms` (`data-form`, `name` attributes and the submit button inside the form, so
 *   behaviour and e2e hooks are unchanged).
 * - `ExamRoleGate` — the "this screen isn't available" tile for the wrong role.
 * - `SessionSelect` — the examination-session picker (styled select).
 */
import { BentoGrid, BentoTile, Checkbox, DatePicker, FormGrid, FormSelect, RowActions, StepDialog, TextField, type ListRowProps, type MenuItem } from "@sutra/ui";
import { useState, type FormEvent, type ReactNode } from "react";
import { useSession as useAuthSession } from "../../lib/auth";
import type { Role } from "../../lib/roles";
import { missingRequired, type AskSpec, type FieldSpec, type FormValues, type RowAction } from "../institution/forms";
import { fieldErrors, problemText } from "../institution/hooks";

/** Labels of destructive commands: tinted in the kebab menu. */
const DANGER = /^(withdraw|withhold|suspend|retire|reject|cancel|archive|delay|reopen)/i;

/** Splits actions into the ONE pill (`primary` variant, else the first non-destructive) and the kebab menu. */
export function splitActions(actions: RowAction[], disabled?: boolean): { primary?: ListRowProps["primary"]; menu?: MenuItem[] } {
  const lead = actions.find((a) => a.variant === "primary") ?? actions.find((a) => !DANGER.test(a.label));
  const rest = actions.filter((a) => a !== lead);
  const menu: MenuItem[] = rest.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.label) }));
  return {
    primary: lead ? { label: lead.label, onClick: lead.onClick, disabled, ghost: lead.variant !== "primary", "data-action": lead.id } : undefined,
    menu: menu.length ? menu : undefined,
  };
}

/** Row / tile actions: one pill + kebab (`RowActions`). Renders nothing for an empty list. */
export function ExamActions({ actions, disabled, label = "More actions", menuOnly }: { actions: RowAction[]; disabled?: boolean; label?: string; menuOnly?: boolean }) {
  if (actions.length === 0) return null;
  const split = splitActions(actions, disabled);
  const primary = menuOnly ? undefined : split.primary;
  const menu = menuOnly ? actions.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.label) })) : split.menu;
  return (
    <span data-actions="" style={{ display: "inline-flex" }}>
      <RowActions label={label} primary={primary} menu={menu} />
    </span>
  );
}

function AskForm({ spec, onDone, onCancel }: { spec: AskSpec & { eyebrow?: string; intro?: ReactNode }; onDone: () => void; onCancel: () => void }) {
  const initial = () => Object.fromEntries(spec.fields.map((f) => [f.name, f.initial ?? (f.type === "checkbox" ? "false" : "")]));
  const [values, setValues] = useState<FormValues>(initial);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const set = (k: string, v: string) => setValues((s) => ({ ...s, [k]: v }));
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const missing = missingRequired(spec.fields, values);
    setErrors(missing);
    setError(null);
    if (Object.keys(missing).length) return;
    setBusy(true);
    try {
      await spec.onSubmit(values);
      onDone();
    } catch (err) {
      const fe = fieldErrors(err);
      if (Object.keys(fe).length) setErrors(fe);
      else setError(problemText(err));
    } finally {
      setBusy(false);
    }
  };
  const columns = spec.fields.length > 2 ? 2 : 1;
  const field = (f: FieldSpec) => {
    const wide = columns > 1 && (f.wide || f.type === "checkbox");
    if (f.type === "select")
      return <FormSelect key={f.name} label={f.label} name={f.name} value={values[f.name]} wide={wide} options={f.options ?? []} error={errors[f.name]} onChange={(e) => set(f.name, e.target.value)} />;
    if (f.type === "checkbox")
      return (
        <div key={f.name} className={wide ? "sb-field sb-wide" : "sb-field"}>
          <Checkbox label={f.label} name={f.name} checked={values[f.name] === "true"} onChange={(e) => set(f.name, String(e.target.checked))} />
        </div>
      );
    if (f.type === "date") return <DatePicker key={f.name} label={f.label} name={f.name} value={values[f.name]} wide={wide} error={errors[f.name]} onChange={(e) => set(f.name, e.target.value)} />;
    return (
      <TextField
        key={f.name}
        label={f.label}
        name={f.name}
        type={f.type === "number" ? "number" : "text"}
        inputMode={f.type === "number" ? "numeric" : undefined}
        value={values[f.name]}
        placeholder={f.placeholder}
        wide={wide}
        error={errors[f.name]}
        onChange={(e) => set(f.name, e.target.value)}
      />
    );
  };
  return (
    <form onSubmit={submit} noValidate data-form={spec.name} style={{ display: "flex", flexDirection: "column", gap: 18 }}>
      {spec.intro && <p style={{ margin: 0, color: "var(--sb-mute)", fontSize: 14 }}>{spec.intro}</p>}
      <FormGrid columns={columns}>{spec.fields.map(field)}</FormGrid>
      {error && (
        <p className="sb-field-error" role="alert" style={{ margin: 0 }}>
          {error}
        </p>
      )}
      <div className="sb-dialog-foot">
        <button type="submit" className="sb-btn" disabled={busy} data-dialog-primary="">
          {busy ? "Saving…" : spec.submitLabel}
        </button>
        <button type="button" className="sb-btn sb-btn--ghost" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
      </div>
    </form>
  );
}

export type ExamAskSpec = AskSpec & { eyebrow?: string; intro?: ReactNode };

/** `ask(spec)` opens the prototype-style dialog; render `prompt` once in the screen. */
export function useExamAsk() {
  const [spec, setSpec] = useState<ExamAskSpec | null>(null);
  const close = () => setSpec(null);
  const prompt = spec ? (
    <StepDialog open onClose={close} eyebrow={spec.eyebrow ?? "Examinations"} title={spec.title} footer={<></>} data-exam-dialog={spec.name}>
      <AskForm key={spec.name + spec.title} spec={spec} onDone={close} onCancel={close} />
    </StepDialog>
  ) : null;
  return { ask: setSpec, prompt };
}

/** Polite tile for a role that may not open the screen (the API enforces the same rule). */
export function ExamRoleGate({ screen, roles, title, note, children }: { screen: string; roles: readonly Role[]; title: string; note: string; children: (role: Role) => ReactNode }) {
  const { role } = useAuthSession();
  if (!roles.includes(role))
    return (
      <BentoGrid data-screen={screen}>
        <BentoTile span={12} variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>
            {title} {note}
          </p>
        </BentoTile>
      </BentoGrid>
    );
  return <BentoGrid data-screen={screen}>{children(role)}</BentoGrid>;
}

/** Examination-session picker (styled native select; `data-session-picker` for tests). */
export function SessionSelect({ value, options, onChange, field }: { value: string; options: { value: string; label: string }[]; onChange: (id: string) => void; field?: string }) {
  if (options.length < 2) return null;
  return (
    <div style={{ maxWidth: 420, marginBottom: 12 }}>
      <FormSelect label="Examination session" value={value} options={options} onChange={(e) => onChange(e.target.value)} data-session-picker="" data-field={field} />
    </div>
  );
}

/** "09:30" + 180 min → "09:30–12:30". */
export function timeRange(startsAt: string, minutes: number | null | undefined): string {
  const m = /^(\d{1,2}):(\d{2})/.exec(startsAt);
  if (!m) return startsAt;
  const start = Number(m[1]) * 60 + Number(m[2]);
  const pad = (n: number) => String(n).padStart(2, "0");
  const fmt = (t: number) => `${pad(Math.floor(t / 60) % 24)}:${pad(t % 60)}`;
  return minutes ? `${fmt(start)}–${fmt(start + minutes)}` : fmt(start);
}
