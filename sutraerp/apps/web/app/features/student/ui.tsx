/**
 * Re-skin v2 (R2) building blocks of the Students area, replacing the Batch 2 `institution/forms`
 * helpers inside `features/student/**` only (docs/reskin2-contracts.md §2.2, §2.4):
 *
 * - `FormDialog` / `useAsk` — prototype dialogs (`StepDialog` look: eyebrow, h2, round close X,
 *   primary pill + ghost Cancel) with styled fields (`TextField`, `FormSelect`, `DatePicker`,
 *   `TagInput`, `Checkbox`, `TextArea`). The `<form data-form=…>` wraps fields AND footer, so
 *   `[data-form] button[type=submit]` and form submits keep working (e2e + unit tests).
 * - `Actions` — ONE primary pill + kebab menu (`RowActions`) instead of clusters of text buttons.
 *   The primary keeps `data-action=<id>`; menu items carry `data-menu-item=<id>`.
 */
import { Building2, X } from "lucide-react";
import {
  Checkbox,
  DatePicker,
  EmptyState,
  FormGrid,
  FormSelect,
  Modal,
  RowActions,
  Stepper,
  TagInput,
  TextArea,
  TextField,
  formatDate,
  type MenuItem,
} from "@sutra/ui";
import { useState, type ReactNode } from "react";
import "./student.css";
import { inputAttrs, useValidatedForm, validateFields, type FieldRule } from "../../lib/forms";

export interface FieldSpec {
  name: string;
  label: string;
  type?: "text" | "date" | "number" | "select" | "checkbox" | "tags" | "textarea" | "email" | "tel";
  options?: Array<{ value: string; label: string }>;
  required?: boolean;
  placeholder?: string;
  hint?: string;
  initial?: string;
  /** Spans the whole form row. */
  wide?: boolean;
  /** Native bounds (number inputs); `rule` adds the server caps (lib/forms `LIMITS`). */
  min?: number;
  max?: number;
  /** Shared value rule: amount, seats, mobile, email, dob, integer. */
  rule?: FieldRule;
  /** Server member name(s) this field answers for when they differ (422 mapping). */
  member?: string | string[];
}

export type FormValues = Record<string, string>;

/** Client-side required check (the API stays authoritative and returns 422 details). */
export function missingRequired(fields: FieldSpec[], values: FormValues): Record<string, string> {
  return validateFields(fields.filter((f) => f.type !== "tags"), values);
}

/** One styled field of a spec (native control underneath, `name` kept for forms and tests). */
export function SpecField({ f, value, error, onChange, onBlur }: { f: FieldSpec; value: string; error?: string; onChange: (v: string) => void; onBlur?: () => void }) {
  switch (f.type) {
    case "select":
      return <FormSelect label={f.label} name={f.name} value={value} error={error} hint={f.hint} wide={f.wide} options={f.options ?? []} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />;
    case "date":
      return <DatePicker label={f.label} name={f.name} value={value} error={error} hint={f.hint} wide={f.wide} {...(f.rule === "dob" ? { min: inputAttrs(f).min, max: inputAttrs(f).max } : {})} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />;
    case "checkbox":
      return (
        <div className={f.wide ? "sb-wide" : undefined} style={{ alignSelf: "end" }}>
          <Checkbox label={f.label} name={f.name} checked={value === "true"} onChange={(e) => onChange(String(e.target.checked))} />
        </div>
      );
    case "tags": {
      const list = value ? value.split(",").filter(Boolean) : [];
      return <TagInput label={f.label} name={f.name} wide={f.wide ?? true} hint={f.hint} error={error} value={list} options={f.options} allowCustom={!f.options} onChange={(next) => onChange(next.join(","))} />;
    }
    case "textarea":
      return <TextArea label={f.label} name={f.name} value={value} error={error} hint={f.hint} wide={f.wide ?? true} placeholder={f.placeholder} rows={3} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />;
    default:
      return <TextField label={f.label} name={f.name} type={f.type ?? "text"} {...inputAttrs(f)} value={value} error={error} hint={f.hint} wide={f.wide} placeholder={f.placeholder} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />;
  }
}

function initialValues(fields: FieldSpec[]): FormValues {
  return Object.fromEntries(fields.map((f) => [f.name, f.initial ?? (f.type === "checkbox" ? "false" : "")]));
}

/** The form body + footer of a dialog (or an inline tile form with `inline`). */
export function SpecForm({
  name,
  fields,
  submitLabel,
  onSubmit,
  onCancel,
  children,
  columns = 2,
  inline,
}: {
  name: string;
  fields: FieldSpec[];
  submitLabel: string;
  /** Resolves when done; throw to show the error (422 → inline). */
  onSubmit: (values: FormValues) => Promise<unknown>;
  onCancel?: () => void;
  children?: ReactNode;
  columns?: 1 | 2 | 3;
  /** Prototype `.x-form`: one row, the button at the end (tile forms). */
  inline?: { icon?: ReactNode };
}) {
  // Phase B shared form layer: rules + bounds, on-blur checks, 422 problem → fields, focus.
  const form = useValidatedForm(fields.filter((f) => f.type !== "tags"), () => initialValues(fields));
  const { values, errors, error, busy, set, blur, formRef } = form;
  const submit = form.submit(onSubmit, () => form.setValues(initialValues(fields)));
  if (inline)
    return (
      <form ref={formRef} onSubmit={submit} noValidate data-form={name} className="st-inline-form">
        {fields.map((f) => (
          <SpecField key={f.name} f={f} value={values[f.name] ?? ""} error={errors[f.name]} onChange={(v) => set(f.name, v)} onBlur={() => blur(f.name)} />
        ))}
        <button type="submit" className="sb-btn" disabled={busy} data-dialog-primary="">
          {inline.icon}
          {busy ? "Working…" : submitLabel}
        </button>
        {error && (
          <p className="sb-field-error" role="alert" style={{ flexBasis: "100%" }}>
            {error}
          </p>
        )}
      </form>
    );
  return (
    <form ref={formRef} onSubmit={submit} noValidate data-form={name} style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {fields.length > 0 && (
        <FormGrid columns={fields.length === 1 ? 1 : columns}>
          {fields.map((f) => (
            <SpecField key={f.name} f={f} value={values[f.name] ?? ""} error={errors[f.name]} onChange={(v) => set(f.name, v)} onBlur={() => blur(f.name)} />
          ))}
        </FormGrid>
      )}
      {children}
      {error && (
        <p className="sb-field-error" role="alert">
          {error}
        </p>
      )}
      <div className="sb-dialog-foot">
        <button type="submit" className="sb-btn" disabled={busy} data-dialog-primary="">
          {busy ? "Working…" : submitLabel}
        </button>
        {onCancel && (
          <button type="button" className="sb-btn sb-btn--ghost" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
        )}
      </div>
    </form>
  );
}

/** Prototype dialog frame (`.md`: eyebrow, h2, round close, optional stepper). */
export function DialogFrame({ open, onClose, eyebrow, title, steps, step = 0, children, ...rest }: { open: boolean; onClose: () => void; eyebrow?: string; title: string; steps?: string[]; step?: number; children: ReactNode; [key: `data-${string}`]: string | undefined }) {
  return (
    <Modal open={open} onClose={onClose} label={title} padded={false}>
      <div className="sb-step-dialog" {...rest}>
        <div className="sb-dialog-head">
          <div>
            {eyebrow && <span className="sb-dialog-eyebrow">{eyebrow}</span>}
            <h2>{title}</h2>
          </div>
          <button type="button" className="sb-icon-btn" aria-label="Close" onClick={onClose}>
            <X strokeWidth={1.75} />
          </button>
        </div>
        {steps && steps.length > 1 && <Stepper steps={steps} current={step} />}
        {children}
      </div>
    </Modal>
  );
}

/** A single-step form dialog (replaces `Modal` + `QuickForm`). */
export function FormDialog({ open, onClose, eyebrow = "Students", title, name, fields, submitLabel, onSubmit, children, columns }: { open: boolean; onClose: () => void; eyebrow?: string; title: string; name: string; fields: FieldSpec[]; submitLabel: string; onSubmit: (v: FormValues) => Promise<unknown>; children?: ReactNode; columns?: 1 | 2 | 3 }) {
  if (!open) return null;
  return (
    <DialogFrame open onClose={onClose} eyebrow={eyebrow} title={title}>
      <SpecForm
        name={name}
        fields={fields}
        submitLabel={submitLabel}
        columns={columns}
        onCancel={onClose}
        onSubmit={async (v) => {
          await onSubmit(v);
          onClose();
        }}
      >
        {children}
      </SpecForm>
    </DialogFrame>
  );
}

/** A small dialog asked before a command (reasons, effective dates). */
export interface AskSpec {
  title: string;
  /** "Students · Admissions" (defaults to "Students"). */
  eyebrow?: string;
  /** `data-form` attribute (tests). */
  name: string;
  fields: FieldSpec[];
  submitLabel: string;
  /** Throw to keep the dialog open and show the error inline. */
  onSubmit: (values: FormValues) => Promise<unknown>;
}

/** `ask(spec)` opens the dialog; render `prompt` once in the screen. */
export function useAsk() {
  const [spec, setSpec] = useState<AskSpec | null>(null);
  const prompt = spec ? (
    <FormDialog key={spec.name} open onClose={() => setSpec(null)} eyebrow={spec.eyebrow} title={spec.title} name={spec.name} fields={spec.fields} submitLabel={spec.submitLabel} onSubmit={spec.onSubmit} />
  ) : null;
  return { ask: setSpec, prompt };
}

/** Reason field (most governed commands require one). */
export const REASON: FieldSpec = { name: "reason", label: "Reason", required: true, wide: true, type: "textarea", placeholder: "Why, in a sentence" };

export interface RowAction {
  label: string;
  onClick: () => void;
  /** `data-action` (primary) / `data-menu-item` (kebab) value. */
  id: string;
  variant?: "primary" | "ghost" | "accent";
  danger?: boolean;
}

const DANGER = /(reject|cancel|withdraw|revoke|suspend|archive|drop|discard|retire|release|expire|fail|waive|remove|close)/;

/** One primary pill (the `primary` action, else the first) + a kebab with the rest. */
export function Actions({ actions, disabled, label = "More actions" }: { actions: RowAction[]; disabled?: boolean; label?: string }) {
  if (actions.length === 0) return null;
  const primary = actions.find((a) => a.variant === "primary") ?? actions[0]!;
  const rest = actions.filter((a) => a !== primary);
  const menu: MenuItem[] = rest.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: a.danger ?? DANGER.test(a.id) }));
  return <RowActions primary={{ label: primary.label, onClick: primary.onClick, disabled, ghost: primary.variant !== "primary" && rest.length === 0 && DANGER.test(primary.id), "data-action": primary.id }} menu={menu} label={label} />;
}

/** `ListRow` props (`primary` + `menu`) from row actions. */
export function rowProps(actions: RowAction[], disabled?: boolean): { primary?: { label: string; onClick: () => void; disabled?: boolean; ghost?: boolean; "data-action": string }; menu?: MenuItem[] } {
  if (actions.length === 0) return {};
  const primary = actions.find((a) => a.variant === "primary") ?? actions[0]!;
  const rest = actions.filter((a) => a !== primary);
  return { primary: { label: primary.label, onClick: primary.onClick, disabled, ghost: primary.variant !== "primary" && rest.length === 0 && DANGER.test(primary.id), "data-action": primary.id }, menu: rest.length ? toMenu(rest, disabled) : undefined };
}

/** Menu items for a kebab / header "More" from row actions. */
export function toMenu(actions: RowAction[], disabled?: boolean): MenuItem[] {
  return actions.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: a.danger ?? DANGER.test(a.id) }));
}

export function NoInstitution() {
  return (
    <EmptyState icon={Building2} title="No institution selected">
      Register an institution in Campus setup → Institution first.
    </EmptyState>
  );
}

/** `YYYY-MM-DD` → "1 Jun 2026". */
export function fmtDate(d: string | null | undefined): string {
  return formatDate(d);
}

/** Today as `YYYY-MM-DD` (browser-local; the API applies the institution's business timezone). */
export function todayIso(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** `12` → "12 credits", `1` → "1 credit". */
export function creditsText(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  const v = Number.isInteger(n) ? String(n) : String(Number(n.toFixed(1)));
  return `${v} ${n === 1 ? "credit" : "credits"}`;
}
