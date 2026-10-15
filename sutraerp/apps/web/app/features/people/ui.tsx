/**
 * Re-skin v2 (R6) building blocks shared by the People screens (docs/reskin2-contracts.md):
 *
 * - `useAsk` — the institution `useAsk` contract (same `AskSpec`, same `data-form` names and field
 *   `name`s) rendered as a prototype `StepDialog` with the R0 form controls (`FormSelect`,
 *   `DatePicker`, `TextField`, `Checkbox`) instead of the old modal + plain inputs.
 * - `Acts` — a row / tile action set as ONE primary pill + a kebab menu (`RowActions`); every
 *   action keeps its id (`data-action` on the pill, `data-menu-item` in the menu).
 * - `days`, `hours`, `inr` — formatters for the decimal strings the API sends ("12.00" → "12 days").
 */
import { Checkbox, DatePicker, FormGrid, FormSelect, RowActions, StepDialog, TextField, formatDays, formatINR, formatNumber, type MenuItem } from "@sutra/ui";
import { useState, type FormEvent, type ReactNode } from "react";
import { missingRequired, type AskSpec, type FieldSpec, type FormValues, type RowAction } from "../institution/forms";
import { fieldErrors, problemText } from "../institution/hooks";

export { type AskSpec, type FieldSpec, type RowAction };

/** "12.00" → "12 days", "0.50" → "0.5 days", "1.00" → "1 day". */
export const days = (v: string | number | null | undefined) => formatDays(v);
/** "12.00" → "12 h". */
export const hours = (v: string | number | null | undefined) => (v == null || v === "" ? "—" : `${formatNumber(v, 1)} h`);
/** Rupees, grouped the Indian way ("41800.00" → "₹41,800"). */
export const inr = (v: string | number | null | undefined) => formatINR(v);

const DANGER = /(cancel|reject|suspend|archive|retire|close|decline|freeze|end|return)$/;

/** Splits actions into the ONE primary pill and the kebab menu (`ListRow` / `RowActions` props). */
export function splitActions(actions: RowAction[], disabled?: boolean, explicitOnly = false): { primary?: { label: string; onClick: () => void; disabled?: boolean; "data-action": string }; menu: MenuItem[] } {
  const primary = actions.find((a) => a.variant === "primary" || a.variant === "accent") ?? (explicitOnly ? undefined : actions.find((a) => !DANGER.test(a.id)));
  const menu: MenuItem[] = actions.filter((a) => a !== primary).map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) }));
  return { primary: primary ? { label: primary.label, onClick: primary.onClick, disabled, "data-action": primary.id } : undefined, menu };
}

/**
 * ONE primary pill + kebab (contracts §2.2). The primary is the first action marked primary, else
 * the first non-destructive one; destructive actions only live in the menu.
 */
export function Acts({ actions, disabled, label = "More actions" }: { actions: RowAction[]; disabled?: boolean; label?: string }) {
  if (actions.length === 0) return null;
  const { primary, menu } = splitActions(actions, disabled);
  return (
    <span className="sb-row-acts" data-acts="">
      <RowActions primary={primary} menu={menu} label={label} />
    </span>
  );
}

/** The dialog form of `useAsk` (StepDialog body + footer inside the `<form>` so Enter submits). */
function AskForm({ spec, onDone }: { spec: AskSpec; onDone: () => void }) {
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
      setBusy(false);
    }
  };
  const columns = spec.fields.filter((f) => f.type !== "checkbox").length > 1 ? 2 : 1;
  return (
    <form onSubmit={submit} noValidate data-form={spec.name}>
      <FormGrid columns={columns}>{spec.fields.map((f) => fieldControl(f, values[f.name] ?? "", errors[f.name], (v) => set(f.name, v)))}</FormGrid>
      {error && (
        <p className="sb-field-error" role="alert">
          {error}
        </p>
      )}
      <div className="sb-dialog-foot">
        <button type="submit" className="sb-btn" disabled={busy} data-dialog-primary="">
          {busy ? "Working…" : spec.submitLabel}
        </button>
        <button type="button" className="sb-btn sb-btn--ghost" onClick={onDone} disabled={busy}>
          Cancel
        </button>
      </div>
    </form>
  );
}

/** One R0 control for a `FieldSpec` (native control + `name` underneath, so e2e `fillForm` works). */
export function fieldControl(f: FieldSpec, value: string, error: string | undefined, onChange: (v: string) => void): ReactNode {
  const wide = f.wide;
  switch (f.type) {
    case "select":
      return <FormSelect key={f.name} label={f.label} name={f.name} value={value} options={f.options ?? []} error={error} wide={wide} onChange={(e) => onChange(e.target.value)} />;
    case "checkbox":
      return (
        <div key={f.name} className="sb-wide">
          <Checkbox label={f.label} name={f.name} checked={value === "true"} onChange={(e) => onChange(String(e.target.checked))} />
        </div>
      );
    case "date":
      return <DatePicker key={f.name} label={f.label} name={f.name} value={value} error={error} wide={wide} onChange={(e) => onChange(e.target.value)} />;
    default:
      return <TextField key={f.name} label={f.label} name={f.name} type={f.type === "number" ? "number" : "text"} value={value} placeholder={f.placeholder} error={error} wide={wide} onChange={(e) => onChange(e.target.value)} />;
  }
}

/** `ask(spec)` opens a prototype dialog ("People · <title>"); render `prompt` once in the screen. */
export function useAsk(eyebrow = "People") {
  const [spec, setSpec] = useState<AskSpec | null>(null);
  const prompt = spec ? (
    <StepDialog open onClose={() => setSpec(null)} eyebrow={eyebrow} title={spec.title} footer={<></>}>
      <AskForm key={spec.name + spec.title} spec={spec} onDone={() => setSpec(null)} />
    </StepDialog>
  ) : null;
  return { ask: setSpec, prompt };
}
