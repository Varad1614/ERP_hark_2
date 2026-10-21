/**
 * Shared form layer (audit 2026-10 Phase B: VAL-2..4, L-1, H-2 web side).
 *
 * One place for what every declarative form in the app does the same way:
 *
 * - `LIMITS` mirror the server caps (A2): seats 1–10 000, one payment ≤ ₹1 crore, applicant age
 *   3–100, Indian mobile, `local@domain.tld` e-mail. The server stays authoritative.
 * - `inputAttrs(field)` gives a field its native `type` / `min` / `max` / `step` / `inputMode`.
 * - `validateField` / `validateFields` run on blur and before submit (required + rule + range).
 * - `problemFieldErrors(err, names)` maps an RFC 9457 problem's `validationErrors[]` (or `errors[]`,
 *   by `field` or JSON `pointer`) onto the form's field names; members the form does not show
 *   come back as `unmatched` so the caller can show them in the form-level message.
 * - `problemMessage(err)` is the human sentence (problem `detail`, never "Request failed (422)").
 * - `focusFirstInvalid(form, order, errors)` focuses the first invalid control in field order.
 */
import { ApiProblem, GENERIC_TITLE, statusMessage } from "@sutra/api-client";
import { useCallback, useRef, useState, type FormEvent, type RefObject } from "react";

export const LIMITS = {
  /** `MAX_SEATS` (academic offering / section capacity). */
  seats: { min: 1, max: 10_000 },
  /** `MAX_TRANSACTION_AMOUNT` in rupees (payments, receipts, refunds). */
  amount: { min: 0.01, max: 10_000_000 },
  /** Applicant age on the business date (admission intake). */
  age: { min: 3, max: 100 },
} as const;

/** Value rules a field may carry on top of `required` / `min` / `max`. */
export type FieldRule = "amount" | "seats" | "mobile" | "email" | "dob" | "integer";

/** The part of a form field spec the shared layer reads (every feature's `FieldSpec` fits). */
export interface ValidatedField {
  name: string;
  label: string;
  type?: string;
  required?: boolean;
  min?: number;
  max?: number;
  rule?: FieldRule;
  /** Server member name(s) this field answers for when they differ (`fullName` ← givenName). */
  member?: string | string[];
}

export type Values = Record<string, string>;
export type Errors = Record<string, string>;

const MOBILE = /^(?:\+?91|0)?[6-9]\d{9}$/;
const EMAIL = /^[^\s@<>()[\]\\,;:"]+@[^\s@<>()[\]\\,;:"]+\.[A-Za-z]{2,}$/;
const AMOUNT = /^\d+(?:\.\d{1,2})?$/;

function rangeOf(f: ValidatedField): { min?: number; max?: number } {
  const base: { min?: number; max?: number } = f.rule === "seats" ? LIMITS.seats : f.rule === "amount" ? LIMITS.amount : {};
  return { min: f.min ?? base.min, max: f.max ?? base.max };
}

/** Native input attributes for a field (number inputs carry the server caps). */
export function inputAttrs(f: ValidatedField): { type?: string; min?: number | string; max?: number | string; step?: string; inputMode?: "decimal" | "numeric" | "tel" | "email" } {
  const { min, max } = rangeOf(f);
  if (f.rule === "amount") return { type: "number", min, max, step: "0.01", inputMode: "decimal" };
  if (f.rule === "seats" || f.rule === "integer" || f.type === "number") return { type: "number", min, max, step: f.rule === "seats" || f.rule === "integer" ? "1" : undefined, inputMode: "numeric" };
  if (f.rule === "mobile") return { type: "tel", inputMode: "tel" };
  if (f.rule === "email") return { type: "email", inputMode: "email" };
  if (f.rule === "dob") return { type: "date", max: isoDate(yearsAgo(LIMITS.age.min)), min: isoDate(yearsAgo(LIMITS.age.max)) };
  return {};
}

function yearsAgo(n: number, today = new Date()): Date {
  return new Date(Date.UTC(today.getFullYear() - n, today.getMonth(), today.getDate()));
}
function isoDate(d: Date): string {
  return d.toISOString().slice(0, 10);
}
const fmt = (n: number) => n.toLocaleString("en-IN");

/** First problem with one field's value, or `undefined` when it is fine. */
export function validateField(f: ValidatedField, raw: string | undefined, today = new Date()): string | undefined {
  const v = (raw ?? "").trim();
  if (f.type === "checkbox") return undefined;
  if (!v) return f.required ? `${f.label} is required` : undefined;
  const { min, max } = rangeOf(f);
  if (f.rule === "amount") {
    if (!AMOUNT.test(v)) return `${f.label} must be an amount in rupees with at most 2 decimals`;
    const n = Number(v);
    if (n <= 0) return `${f.label} must be more than ₹0`;
    if (max !== undefined && n > max) return `${f.label} can be at most ₹${fmt(max)}`;
    return undefined;
  }
  if (f.rule === "seats" || f.rule === "integer" || f.type === "number") {
    const n = Number(v);
    if (!Number.isFinite(n)) return `${f.label} must be a number`;
    if ((f.rule === "seats" || f.rule === "integer") && !Number.isInteger(n)) return `${f.label} must be a whole number`;
    if (min !== undefined && max !== undefined && (n < min || n > max)) return `${f.label} must be from ${fmt(min)} to ${fmt(max)}`;
    if (min !== undefined && n < min) return `${f.label} must be at least ${fmt(min)}`;
    if (max !== undefined && n > max) return `${f.label} can be at most ${fmt(max)}`;
    return undefined;
  }
  if (f.rule === "mobile" && !MOBILE.test(v.replace(/[\s().-]/g, ""))) return `${f.label} must be a 10-digit Indian mobile number starting with 6–9`;
  if (f.rule === "email" && (v.length > 254 || !EMAIL.test(v))) return `${f.label} must be an e-mail address like name@example.com`;
  if (f.rule === "dob") {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(v) || Number.isNaN(Date.parse(v))) return `${f.label} must be a date`;
    if (v > isoDate(today)) return `${f.label} cannot be in the future`;
    if (v > isoDate(yearsAgo(LIMITS.age.min, today)) || v < isoDate(yearsAgo(LIMITS.age.max, today))) return `Applicant must be ${LIMITS.age.min}–${LIMITS.age.max} years old`;
  }
  return undefined;
}

/** All field problems (required first, then rules). */
export function validateFields(fields: ValidatedField[], values: Values, today = new Date()): Errors {
  const e: Errors = {};
  for (const f of fields) {
    const m = validateField(f, values[f.name], today);
    if (m) e[f.name] = m;
  }
  return e;
}

interface Violation {
  field?: string;
  pointer?: string;
  name?: string;
  message?: string;
  detail?: string;
}

function violations(err: unknown): Violation[] {
  const p = err instanceof ApiProblem ? (err.problem as unknown as Record<string, unknown>) : typeof err === "object" && err !== null && "problem" in err ? ((err as { problem: Record<string, unknown> }).problem ?? {}) : undefined;
  if (!p) return [];
  const list = (Array.isArray(p.validationErrors) ? p.validationErrors : Array.isArray(p.errors) ? p.errors : []) as Violation[];
  return list.filter((v) => typeof v === "object" && v !== null);
}

const camel = (s: string) => s.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());

/**
 * Candidate form names for a server member: `"/body/applicant/date_of_birth"`, `"#/amount"`,
 * `"allocations[0].amount"`, `"sections.0.capacity"` → full dotted path, then the leaf.
 */
export function memberNames(member: string): string[] {
  const parts = member
    .replace(/^#/, "")
    .split(/[/.[\]]+/)
    .filter((s) => s && s !== "body" && !/^\d+$/.test(s))
    .map(camel);
  if (parts.length === 0) return [];
  const out = [parts.join("."), parts[parts.length - 1]!];
  return [...new Set(out)];
}

/** `member` → form field name map of `fields` (for `problemFieldErrors`). */
export function memberAliases(fields: ValidatedField[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const f of fields) for (const m of f.member === undefined ? [] : Array.isArray(f.member) ? f.member : [f.member]) out[m] = f.name;
  return out;
}

/** Server field violations mapped onto `names` (the form's fields), plus those it cannot show. */
export function problemFieldErrors(err: unknown, names: string[], aliases: Record<string, string> = {}): { fields: Errors; unmatched: string[] } {
  const known = new Set(names);
  const fields: Errors = {};
  const unmatched: string[] = [];
  for (const v of violations(err)) {
    const message = v.message ?? v.detail ?? "Not valid";
    const member = v.field ?? v.pointer ?? v.name ?? "";
    const target = memberNames(member)
      .map((n) => aliases[n] ?? n)
      .find((n) => known.has(n));
    if (target) {
      if (!fields[target]) fields[target] = message;
    } else unmatched.push(message);
  }
  return { fields, unmatched };
}

/** Human message of a failed request: problem `detail`, else a status sentence (never "Request failed (n)"). */
export function problemMessage(err: unknown): string {
  if (err instanceof ApiProblem) {
    const p = err.problem;
    if (p.detail) return p.detail;
    if (p.title && !GENERIC_TITLE.test(p.title) && p.status !== 422) return p.title;
    return statusMessage(p.status);
  }
  if (err instanceof Error) {
    const m = /^Request failed \((\d+)\)$/.exec(err.message);
    if (m) return statusMessage(Number(m[1]));
    if (err.message === "Request failed") return statusMessage(0);
    return err.message;
  }
  return "Something went wrong. Try again.";
}

/** Focuses the first control (in field order) that has an error. Returns its name. */
export function focusFirstInvalid(form: HTMLElement | null, order: string[], errors: Errors): string | undefined {
  const first = order.find((n) => errors[n]);
  if (!form || !first) return first;
  const el = form.querySelector<HTMLElement>(`[name="${CSS.escape(first)}"]`);
  el?.focus();
  return first;
}

function omit(e: Errors, k: string): Errors {
  if (!(k in e)) return e;
  const next = { ...e };
  delete next[k];
  return next;
}

/**
 * State + handlers of a validated form. `fields` gives order and rules; `blur(name)` validates
 * one field; `submit(onSubmit)` validates all, focuses the first invalid control, runs `onSubmit`
 * and maps a 422 problem onto the fields (form-level message for the rest).
 */
export function useValidatedForm<F extends ValidatedField>(fields: F[], initial: () => Values, opts: { aliases?: Record<string, string>; extraCheck?: (values: Values) => Errors } = {}) {
  const [values, setValues] = useState<Values>(initial);
  const [errors, setErrors] = useState<Errors>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const formRef = useRef<HTMLFormElement | null>(null) as RefObject<HTMLFormElement | null>;
  const valuesRef = useRef(values);
  valuesRef.current = values;
  const order = fields.map((f) => f.name);

  const set = useCallback((k: string, v: string) => {
    setValues((s) => ({ ...s, [k]: v }));
    setErrors((e) => omit(e, k));
  }, []);

  const blur = (name: string) => {
    const f = fields.find((x) => x.name === name);
    if (!f) return;
    const m = validateField(f, valuesRef.current[name]);
    setErrors((e) => (m ? { ...e, [name]: m } : omit(e, name)));
  };

  const show = (e: Errors) => {
    setErrors(e);
    // after paint, so a field that only now shows its error is focusable
    queueMicrotask(() => focusFirstInvalid(formRef.current, order, e));
  };

  const submit = (onSubmit: (values: Values) => Promise<unknown>, after?: () => void) => async (ev?: FormEvent) => {
    ev?.preventDefault();
    const e = { ...validateFields(fields, values), ...(opts.extraCheck?.(values) ?? {}) };
    setError(null);
    if (Object.keys(e).length) return show(e);
    setErrors({});
    setBusy(true);
    try {
      await onSubmit(values);
      after?.();
    } catch (err) {
      const { fields: fe, unmatched } = problemFieldErrors(err, order, { ...memberAliases(fields), ...opts.aliases });
      if (Object.keys(fe).length) show(fe);
      if (unmatched.length || !Object.keys(fe).length) setError(unmatched[0] ?? problemMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return { values, setValues, set, errors, setErrors, error, setError, busy, blur, submit, formRef, attrs: (f: F) => inputAttrs(f) };
}
