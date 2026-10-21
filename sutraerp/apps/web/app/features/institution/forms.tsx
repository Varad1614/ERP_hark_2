/**
 * Small building blocks shared by the Batch 2 tabs: a declarative quick form (inline 422 field
 * errors from the API), an action button row and the "pick an institution first" state.
 */
import { Building2 } from "lucide-react";
import { Button, EmptyState, Input, Modal, Select, type SelectOption } from "@sutra/ui";
import { useState, type ReactNode } from "react";
import { inputAttrs, useValidatedForm, validateFields, type FieldRule } from "../../lib/forms";

export interface FieldSpec {
  name: string;
  label: string;
  type?: "text" | "date" | "number" | "select" | "checkbox";
  options?: SelectOption[];
  required?: boolean;
  placeholder?: string;
  initial?: string;
  /** Spans the whole form row. */
  wide?: boolean;
  /** Native bounds (number inputs); `rule` adds the server caps (lib/forms `LIMITS`). */
  min?: number;
  max?: number;
  /** Shared value rule: amount (₹, ≤ 1 crore), seats (1–10 000), mobile, email, dob, integer. */
  rule?: FieldRule;
  /** Server member name(s) this field answers for when they differ (422 mapping). */
  member?: string | string[];
}

export type FormValues = Record<string, string>;

/**
 * Client-side check: required, then the field's rule / bounds (lib/forms). The API stays
 * authoritative and returns 422 details, which the forms map back onto the fields.
 */
export function missingRequired(fields: FieldSpec[], values: FormValues): Record<string, string> {
  return validateFields(fields, values);
}

export function QuickForm({
  fields,
  submitLabel,
  onSubmit,
  onCancel,
  name,
  children,
}: {
  fields: FieldSpec[];
  submitLabel: string;
  /** Resolves when done; throw to show the error (422 → inline). */
  onSubmit: (values: FormValues) => Promise<unknown>;
  onCancel?: () => void;
  /** `data-form` attribute (tests). */
  name: string;
  children?: ReactNode;
}) {
  const initial = () => Object.fromEntries(fields.map((f) => [f.name, f.initial ?? (f.type === "checkbox" ? "false" : "")]));
  const form = useValidatedForm(fields, initial);
  const { errors, error, busy, set, blur, formRef } = form;
  const values = form.values;
  const submit = form.submit(onSubmit, () => form.setValues(initial()));

  return (
    <form ref={formRef} onSubmit={submit} noValidate data-form={name}>
      <div className="sb-form">
        {fields.map((f) =>
          f.type === "select" ? (
            <div key={f.name} className={f.wide ? "sb-span-all" : undefined}>
              <Select label={f.label} name={f.name} value={values[f.name]} options={f.options ?? []} error={errors[f.name]} onChange={(e) => set(f.name, e.target.value)} onBlur={() => blur(f.name)} />
            </div>
          ) : f.type === "checkbox" ? (
            <label key={f.name} className="sb-field">
              <span>
                <input type="checkbox" name={f.name} checked={values[f.name] === "true"} onChange={(e) => set(f.name, String(e.target.checked))} /> {f.label}
              </span>
            </label>
          ) : (
            <div key={f.name} className={f.wide ? "sb-span-all" : undefined}>
              <Input
                label={f.label}
                name={f.name}
                type={f.type ?? "text"}
                {...inputAttrs(f)}
                value={values[f.name]}
                placeholder={f.placeholder}
                error={errors[f.name]}
                onChange={(e) => set(f.name, e.target.value)}
                onBlur={() => blur(f.name)}
              />
            </div>
          ),
        )}
      </div>
      {children}
      {error && (
        <p className="sb-field-error" role="alert">
          {error}
        </p>
      )}
      <div className="sb-actions-row">
        <Button type="submit" disabled={busy}>
          {busy ? "Saving…" : submitLabel}
        </Button>
        {onCancel && (
          <Button variant="ghost" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
        )}
      </div>
    </form>
  );
}

export interface RowAction {
  label: string;
  onClick: () => void;
  /** `data-action` value (tests). */
  id: string;
  variant?: "primary" | "ghost" | "accent";
}

export function Actions({ actions, disabled }: { actions: RowAction[]; disabled?: boolean }) {
  if (actions.length === 0) return null;
  return (
    <div className="sb-row-actions">
      {actions.map((a) => (
        <Button key={a.id} size="sm" variant={a.variant ?? "ghost"} onClick={a.onClick} disabled={disabled} data-action={a.id}>
          {a.label}
        </Button>
      ))}
    </div>
  );
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
  if (!d) return "—";
  const [y, m, day] = d.split("-").map(Number);
  if (!y || !m || !day) return d;
  return new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" }).format(new Date(Date.UTC(y, m - 1, day)));
}

/** A small modal form asked before a command (reasons, effective dates). */
export interface AskSpec {
  title: string;
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
    <Modal open onClose={() => setSpec(null)} label={spec.title}>
      <h2>{spec.title}</h2>
      <QuickForm
        name={spec.name}
        fields={spec.fields}
        submitLabel={spec.submitLabel}
        onCancel={() => setSpec(null)}
        onSubmit={async (v) => {
          await spec.onSubmit(v);
          setSpec(null);
        }}
      />
    </Modal>
  ) : null;
  return { ask: setSpec, prompt };
}

/** Reason field (most governed commands require one). */
export const REASON: FieldSpec = { name: "reason", label: "Reason", required: true, wide: true };

/** Today as `YYYY-MM-DD` (browser-local; the API applies the institution's business timezone). */
export function todayIso(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
