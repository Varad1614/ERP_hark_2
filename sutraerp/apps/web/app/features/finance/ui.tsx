/**
 * Re-skin v2 (R5) building blocks shared by the Finance screens: the area frame (`FinanceFrame`,
 * prototype `.ph` with ≤5 header tabs + More), money formatting through the R0 `formatINR`, the
 * prototype dialog form (`useFinanceAsk`, a `StepDialog` with the R0 form controls) and the
 * one-primary-plus-kebab row actions (`rowActions`).
 */
import { useQuery } from "@tanstack/react-query";
import {
  AreaHeader,
  BentoGrid,
  BentoTile,
  Checkbox,
  CheckboxList,
  DatePicker,
  FormGrid,
  FormSelect,
  RowActions,
  StepDialog,
  TextField,
  formatINR,
  formatPercent,
  type MenuItem,
  type NavLinkLikeProps,
} from "@sutra/ui";
import { useId, useState, type ReactNode } from "react";
import { NavLink } from "react-router";
import { useSession } from "../../lib/auth";
import { inputAttrs, useValidatedForm } from "../../lib/forms";
import type { FieldSpec, FormValues, RowAction } from "../institution/forms";
import { selectInstitution, useInstitutions } from "../institution/hooks";
import type { Institution } from "../institution/types";
import { fkeys, useChargesApi } from "./charges/hooks";
import { FINANCE_TABS, activeTab, financePath } from "./index";
import { pkeys, usePaymentsApi } from "./payments/hooks";
import { fromPaise, toPaise } from "./payments/mock";

// ------------------------------------------------------------------ money

/** Rupees, Indian grouping, no needless `.00` (`₹1,25,000`, `₹46,250.50`). */
export const money = (v: string | number | null | undefined) => formatINR(v);
/** Ledger / receipt amounts keep the paise (`₹1,25,000.00`). */
export const ledger = (v: string | number | null | undefined) => formatINR(v, { paise: "always" });
/** KPI / chart short form (`₹3.4 Cr`, `₹92 L`). */
export const compact = (v: string | number | null | undefined) => formatINR(v, { compact: true });

/** Σ of decimal strings in paise (exact, no floats). */
export function sumPaise<T>(rows: readonly T[], pick: (r: T) => string | null | undefined): bigint {
  return rows.reduce((s, r) => s + toPaise(pick(r) || "0"), 0n);
}
export const paiseToAmount = (p: bigint) => fromPaise(p);

/** Charges that are real receivables (published and later; never drafts or cancelled ones). */
export const BILLED = ["published", "active", "disputed", "suspended", "closed", "written_off", "waived"];

export type InvoiceState = "paid" | "pending" | "overdue" | "other";
/** Invoice state of a charge for the chips / badges: paid, overdue (due date passed), pending. */
export function invoiceState(c: { status: string; outstandingAmount: string; dueDate?: string | null }, today: string): InvoiceState {
  if (!BILLED.includes(c.status)) return "other";
  if (toPaise(c.outstandingAmount || "0") <= 0n) return "paid";
  if (c.dueDate && c.dueDate < today) return "overdue";
  return "pending";
}
export const INVOICE_BADGE: Record<Exclude<InvoiceState, "other">, { label: string; kind: "done" | "review" | "attention" }> = {
  paid: { label: "Paid", kind: "done" },
  pending: { label: "Pending", kind: "review" },
  overdue: { label: "Overdue", kind: "attention" },
};

// ------------------------------------------------------------------ area frame

function FinanceNavLink({ to, className, end, children }: NavLinkLikeProps) {
  return (
    <NavLink to={to} end={end} className={typeof className === "function" ? ({ isActive }) => className({ isActive }) : className}>
      {children}
    </NavLink>
  );
}

/** Live one-line summary of the area (collected of billed · invoices · approvals waiting). */
function useFinanceSummary(institution: Institution | null): string | undefined {
  const charges = useChargesApi();
  const payments = usePaymentsApi();
  const iid = institution?.id;
  const list = useQuery({ queryKey: fkeys.scoped(`${iid}:`, "charges"), queryFn: () => charges.charges(iid!), enabled: !!iid });
  const paid = useQuery({ queryKey: pkeys.scoped(iid ?? "-", "payments"), queryFn: () => payments.payments(iid!), enabled: !!iid });
  const adjustments = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "adjustments"), queryFn: () => charges.adjustments(iid!), enabled: !!iid });
  if (!list.data || !paid.data) return undefined;
  const billed = list.data.items.filter((c) => BILLED.includes(c.status));
  const collected = sumPaise(
    paid.data.filter((p) => p.status === "completed"),
    (p) => p.amount,
  );
  const total = sumPaise(billed, (c) => c.totalAmount);
  const waiting = (adjustments.data ?? []).filter((a) => a.status === "pending_approval").length;
  const parts = [`${compact(paiseToAmount(collected))} collected of ${compact(paiseToAmount(total))} billed`, `${billed.length.toLocaleString("en-IN")} ${billed.length === 1 ? "invoice" : "invoices"}`];
  if (waiting) parts.push(`${waiting} ${waiting === 1 ? "adjustment" : "adjustments"} to review`);
  return parts.join(" · ");
}

/**
 * Admin → Finance frame: administrator gate, `AreaHeader` (h1 + live summary; Overview /
 * Collections / Receivables / Ledger + More), the institution picker when there are several,
 * then `children(institution, tab)`.
 */
export function FinanceFrame({ tab, children }: { tab: string | undefined; children: (institution: Institution | null, tab: string) => ReactNode }) {
  const { role } = useSession();
  const current = activeTab(FINANCE_TABS, tab);
  const { list, current: institution, query } = useInstitutions();
  const summary = useFinanceSummary(role === "administrator" ? institution : null);
  if (role !== "administrator") {
    return (
      <BentoGrid data-screen="finance">
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Finance is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  return (
    <BentoGrid data-screen="finance" data-tab={current.key}>
      <AreaHeader
        title="Finance"
        summary={
          institution ? (
            <span data-current-institution={institution.id}>{summary ?? "Loading the fee ledger…"}</span>
          ) : query.isLoading ? (
            "Loading institutions…"
          ) : (
            "No institution registered yet."
          )
        }
        tabs={FINANCE_TABS.map((t) => ({ id: t.key, label: t.label, to: financePath(t.key === "overview" ? undefined : t.key) }))}
        activeTab={current.key}
        linkComponent={FinanceNavLink}
        tabsLabel="Finance sections"
        actions={
          list.length > 1 && institution ? (
            <div style={{ minWidth: 220 }}>
              <FormSelect
                label="Institution"
                hideLabel
                name="institution"
                value={institution.id}
                options={list.map((i) => ({ value: i.id, label: i.name }))}
                onChange={(e) => selectInstitution(e.target.value)}
              />
            </div>
          ) : undefined
        }
      />
      {children(institution, current.key)}
    </BentoGrid>
  );
}

// ------------------------------------------------------------------ row actions

const DANGER = /(cancel|reverse|reject|revoke|write-off|suspend|archive)/;

/** One primary pill (the first `primary`-variant action, else a lone action) + the rest in the kebab. */
export function rowActions(actions: RowAction[], disabled?: boolean): ReactNode {
  if (actions.length === 0) return null;
  const main = actions.find((a) => a.variant === "primary") ?? (actions.length === 1 ? actions[0] : undefined);
  const rest = actions.filter((a) => a !== main);
  const menu: MenuItem[] = rest.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) }));
  return <RowActions primary={main ? { label: main.label, onClick: main.onClick, disabled, ghost: main.variant !== "primary", "data-action": main.id } : undefined} menu={menu.length ? menu : undefined} />;
}

/** `82%` collection rate from paise totals (no floats on money; the ratio is display only). */
export function rate(part: bigint, whole: bigint): string {
  if (whole <= 0n) return "—";
  return formatPercent(Number((part * 1000n) / whole) / 10);
}

// ------------------------------------------------------------------ dialog form

/** A multi-select checkbox list field (replaces comma-separated id text). Values join with ",". */
export interface ChecksField {
  name: string;
  label: string;
  type: "checks";
  options: { value: string; label: string; description?: string }[];
  required?: boolean;
  wide?: boolean;
  initial?: string;
}
export type FinField = FieldSpec | ChecksField;

export interface FinAskSpec {
  title: string;
  /** Eyebrow after "Finance · " (defaults to the record the dialog acts on, or nothing). */
  eyebrow?: string;
  /** `data-form` attribute (tests). */
  name: string;
  fields: FinField[];
  submitLabel: string;
  /** Extra body above the fields (amount, record facts). */
  intro?: ReactNode;
  onSubmit: (values: FormValues) => Promise<unknown>;
}

function initialValues(fields: FinField[]): FormValues {
  return Object.fromEntries(fields.map((f) => [f.name, f.initial ?? (f.type === "checkbox" ? "false" : "")]));
}

function FinanceForm({ spec, onDone }: { spec: FinAskSpec; onDone: () => void }) {
  const formId = useId();
  // Phase B shared form layer: rules + bounds, on-blur checks, 422 problem → fields, focus.
  const plain = spec.fields.filter((f): f is FieldSpec => f.type !== "checks");
  const form = useValidatedForm(plain, () => initialValues(spec.fields), {
    extraCheck: (v) => Object.fromEntries(spec.fields.filter((f) => f.type === "checks" && f.required && !v[f.name]).map((f) => [f.name, "Choose at least one"])),
  });
  const { values, errors, error, busy, set, blur, formRef } = form;
  const submit = form.submit(spec.onSubmit, onDone);
  const field = (f: FinField) => {
    const wide = f.wide;
    if (f.type === "checks") {
      const chosen = new Set((values[f.name] ?? "").split(",").filter(Boolean));
      const toggle = (v: string, on: boolean) => {
        const next = new Set(chosen);
        if (on) next.add(v);
        else next.delete(v);
        set(f.name, [...next].join(","));
      };
      return (
        <div key={f.name} className="sb-wide" data-checks={f.name}>
          <span className="sb-field-label" style={{ display: "block", fontSize: 13, fontWeight: 600, marginBottom: 8 }}>
            {f.label}
          </span>
          {f.options.length === 0 ? (
            <p style={{ color: "var(--sb-mute)", fontSize: 14 }}>Nothing to choose from yet.</p>
          ) : (
            <CheckboxList label={f.label}>
              {f.options.map((o) => (
                <Checkbox key={o.value} name={`${f.name}:${o.value}`} label={o.label} description={o.description} checked={chosen.has(o.value)} onChange={(e) => toggle(o.value, e.target.checked)} />
              ))}
            </CheckboxList>
          )}
          {errors[f.name] && (
            <span className="sb-field-error" role="alert">
              {errors[f.name]}
            </span>
          )}
        </div>
      );
    }
    if (f.type === "select")
      return <FormSelect key={f.name} label={f.label} name={f.name} wide={wide} value={values[f.name]} options={f.options ?? []} error={errors[f.name]} onChange={(e) => set(f.name, e.target.value)} onBlur={() => blur(f.name)} />;
    if (f.type === "checkbox")
      return (
        <div key={f.name} className={wide ? "sb-wide" : undefined}>
          <Checkbox name={f.name} label={f.label} checked={values[f.name] === "true"} onChange={(e) => set(f.name, String(e.target.checked))} />
        </div>
      );
    if (f.type === "date") return <DatePicker key={f.name} label={f.label} name={f.name} wide={wide} value={values[f.name] ?? ""} error={errors[f.name]} onChange={(e) => set(f.name, e.target.value)} onBlur={() => blur(f.name)} />;
    return (
      <TextField
        key={f.name}
        label={f.label}
        name={f.name}
        wide={wide}
        type={f.type === "number" ? "number" : "text"}
        inputMode={/amount|balance|percent/i.test(f.name) ? "decimal" : undefined}
        value={values[f.name] ?? ""}
        placeholder={f.placeholder}
        {...inputAttrs(f)}
        error={errors[f.name]}
        onChange={(e) => set(f.name, e.target.value)}
        onBlur={() => blur(f.name)}
      />
    );
  };
  return (
    <>
      <form id={formId} ref={formRef} onSubmit={submit} noValidate>
        {spec.intro}
        <FormGrid columns={spec.fields.length > 2 ? 2 : 1}>{spec.fields.map(field)}</FormGrid>
        {error && (
          <p className="sb-field-error" role="alert" style={{ marginTop: 12 }}>
            {error}
          </p>
        )}
      </form>
      <div className="sb-dialog-foot">
        <button type="submit" form={formId} className="sb-btn" disabled={busy} data-dialog-primary="">
          {busy ? "Working…" : spec.submitLabel}
        </button>
        <button type="button" className="sb-btn sb-btn--ghost" onClick={onDone} disabled={busy}>
          Cancel
        </button>
      </div>
    </>
  );
}

/** `ask(spec)` opens a prototype dialog (`StepDialog` look) with the form; render `prompt` once. */
export function useFinanceAsk() {
  const [spec, setSpec] = useState<FinAskSpec | null>(null);
  const prompt = spec ? <FinanceDialog spec={spec} onClose={() => setSpec(null)} /> : null;
  return { ask: setSpec, prompt };
}

function FinanceDialog({ spec, onClose }: { spec: FinAskSpec; onClose: () => void }) {
  return (
    <StepDialog open onClose={onClose} eyebrow={`Finance${spec.eyebrow ? ` · ${spec.eyebrow}` : ""}`} title={spec.title} footer={<></>} data-form={spec.name}>
      <FinanceForm spec={spec} onDone={onClose} />
    </StepDialog>
  );
}
