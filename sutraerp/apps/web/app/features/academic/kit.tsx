/**
 * Re-skin v2 (slice R3) building blocks for the Academics area and the teaching screens:
 *
 * - `useAsk` — the declarative quick form of `features/institution/forms` rendered as a prototype
 *   `StepDialog` (eyebrow, h2, round close, `FormGrid` with `FormSelect` / `DatePicker` /
 *   `TextField` / `TextArea` / `Toggle`). Same `AskSpec`, so every caller keeps its API calls; the
 *   dialog wrapper carries `data-form` and the footer pill is the form's submit button (e2e
 *   `fillForm` unchanged).
 * - `AskForm` — the same form inline in a tile (replaces `QuickForm`).
 * - `RowMenu` — a row's actions as ONE pill + a kebab menu (`RowActions`); menu items keep the
 *   action id as `data-menu-item`, the pill keeps `data-action`.
 * - `AcademicFrame` — admin area frame: `AreaHeader` with a live summary, ≤5 tabs + More, the
 *   institution picker; replaces `AdminScreen` (breadcrumb subtitle + long tab strip) here.
 * - `TabLink` helpers, `useTerms`, term / slot / time helpers shared by the R3 screens.
 *
 * Local to the feature (docs/reskin2-contracts.md §1: only R0 edits packages/ui).
 */
import "./academic.css";
import { AreaHeader, BentoGrid, BentoTile, Checkbox, cx, DatePicker, FormGrid, FormSelect, RowActions, StepDialog, TextArea, TextField, Toggle, type AreaTab, type MenuItem, type NavLinkLikeProps, type Tone, type WeekSlot } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useMemo, useState, type ComponentType, type ReactNode } from "react";
import { Link, NavLink } from "react-router";
import { useSession } from "../../lib/auth";
import type { FieldSpec as BaseFieldSpec, FormValues, RowAction } from "../institution/forms";
import { selectInstitution, useInstitutionApi, useInstitutions } from "../institution/hooks";
import type { Institution } from "../institution/types";
import { inputAttrs, useValidatedForm } from "../../lib/forms";

export type { FormValues, RowAction } from "../institution/forms";

// ================================================================== forms

export interface FieldSpec extends Omit<BaseFieldSpec, "type"> {
  type?: BaseFieldSpec["type"] | "textarea" | "toggle" | "time";
  hint?: string;
}

export interface AskSpec {
  title: string;
  /** "Academics · Courses" (area · action). Defaults to nothing. */
  eyebrow?: string;
  /** `data-form` attribute (tests). */
  name: string;
  fields: FieldSpec[];
  submitLabel: string;
  /** Content above the fields (summary cells, a warning line). */
  intro?: ReactNode;
  columns?: 1 | 2 | 3;
  /** Throw to keep the dialog open and show the error inline. */
  onSubmit: (values: FormValues) => Promise<unknown>;
}

const initialValues = (fields: FieldSpec[]) => Object.fromEntries(fields.map((f) => [f.name, f.initial ?? (f.type === "checkbox" || f.type === "toggle" ? "false" : "")]));

/** One field of a spec in the prototype form style. */
function SpecField({ f, value, error, set, blur }: { f: FieldSpec; value: string; error?: string; set: (v: string) => void; blur?: () => void }) {
  const wide = f.wide || f.type === "textarea";
  switch (f.type) {
    case "select":
      return <FormSelect label={f.label} name={f.name} value={value} options={(f.options ?? []).map((o) => ({ value: String(o.value), label: o.label }))} error={error} hint={f.hint} wide={wide} onChange={(e) => set(e.target.value)} />;
    case "date":
      return <DatePicker label={f.label} name={f.name} value={value} error={error} hint={f.hint} wide={wide} onChange={(e) => set(e.target.value)} />;
    case "checkbox":
      return (
        <div className={cx("sb-field", wide && "sb-wide")}>
          <Checkbox label={f.label} name={f.name} checked={value === "true"} onChange={(e) => set(String(e.target.checked))} />
        </div>
      );
    case "toggle":
      return (
        <div className={cx("sb-field", wide && "sb-wide")}>
          <Toggle label={f.label} name={f.name} checked={value === "true"} onChange={(e) => set(String(e.target.checked))} />
        </div>
      );
    case "textarea":
      return <TextArea label={f.label} name={f.name} value={value} placeholder={f.placeholder} error={error} hint={f.hint} wide rows={3} onChange={(e) => set(e.target.value)} />;
    default:
      return (
        <TextField
          label={f.label}
          name={f.name}
          type={f.type === "number" ? "number" : f.type === "time" ? "time" : "text"}
          inputMode={f.type === "number" ? "decimal" : undefined}
          {...inputAttrs(f)}
          onBlur={blur}
          value={value}
          placeholder={f.placeholder}
          error={error}
          hint={f.hint}
          wide={wide}
          onChange={(e) => set(e.target.value)}
        />
      );
  }
}

function useSpecForm(fields: FieldSpec[], onSubmit: (v: FormValues) => Promise<unknown>, resetOnSuccess = true) {
  // Phase B shared form layer: rules + bounds, on-blur checks, 422 problem → fields, focus.
  const checked = fields.filter((f) => f.type !== "toggle");
  const form = useValidatedForm(checked, () => initialValues(fields));
  const set = (k: string) => (v: string) => form.set(k, v);
  const handle = form.submit(onSubmit, resetOnSuccess ? () => form.setValues(initialValues(fields)) : undefined);
  return { values: form.values, errors: form.errors, error: form.error, busy: form.busy, set, handle, blur: form.blur, formRef: form.formRef };
}

function AskDialog({ spec, onClose }: { spec: AskSpec; onClose: () => void }) {
  const f = useSpecForm(
    spec.fields,
    async (v) => {
      await spec.onSubmit(v);
      onClose();
    },
    false,
  );
  // The footer lives inside the <form> (data-form + a real submit button: e2e / unit `fillForm`).
  return (
    <StepDialog open onClose={onClose} eyebrow={spec.eyebrow} title={spec.title} footer={<></>}>
      <form ref={f.formRef} onSubmit={f.handle} noValidate className="ac-ask" data-form={spec.name}>
        {spec.intro}
        {spec.fields.length > 0 && (
          <FormGrid columns={spec.columns ?? (spec.fields.length > 1 ? 2 : 1)}>
            {spec.fields.map((x) => (
              <SpecField key={x.name} f={x} value={f.values[x.name] ?? ""} error={f.errors[x.name]} set={f.set(x.name)} blur={() => f.blur(x.name)} />
            ))}
          </FormGrid>
        )}
        {f.error && (
          <p className="sb-field-error" role="alert">
            {f.error}
          </p>
        )}
        <div className="sb-dialog-foot">
          <button type="submit" className="sb-btn" disabled={f.busy} data-dialog-primary="">
            {f.busy ? "Working…" : spec.submitLabel}
          </button>
          <button type="button" className="sb-btn sb-btn--ghost" onClick={onClose}>
            Cancel
          </button>
        </div>
      </form>
    </StepDialog>
  );
}

/** `ask(spec)` opens the dialog; render `prompt` once in the screen. Drop-in for `institution/forms` `useAsk`. */
export function useAsk() {
  const [spec, setSpec] = useState<AskSpec | null>(null);
  const prompt = spec ? <AskDialog key={spec.name + spec.title} spec={spec} onClose={() => setSpec(null)} /> : null;
  return { ask: setSpec, prompt };
}

/** Inline form in a tile (prototype `.x-lf` / `.x-g2`), replaces `QuickForm`. */
export function AskForm({ fields, submitLabel, onSubmit, onCancel, name, columns, children, title, bare }: { fields: FieldSpec[]; submitLabel: string; onSubmit: (v: FormValues) => Promise<unknown>; onCancel?: () => void; name: string; columns?: 1 | 2 | 3; children?: ReactNode; title?: string; bare?: boolean }) {
  const f = useSpecForm(fields, onSubmit);
  return (
    <form ref={f.formRef} onSubmit={f.handle} noValidate data-form={name} className={bare ? "ac-ask" : "ac-inline-form"}>
      {title && <h3 className="ac-form-title">{title}</h3>}
      <FormGrid columns={columns ?? (fields.length >= 3 ? 3 : 2)}>
        {fields.map((x) => (
          <SpecField key={x.name} f={x} value={f.values[x.name] ?? ""} error={f.errors[x.name]} set={f.set(x.name)} blur={() => f.blur(x.name)} />
        ))}
      </FormGrid>
      {children}
      {f.error && (
        <p className="sb-field-error" role="alert">
          {f.error}
        </p>
      )}
      <div className="ac-form-foot">
        <button type="submit" className="sb-btn" disabled={f.busy}>
          {f.busy ? "Saving…" : submitLabel}
        </button>
        {onCancel && (
          <button type="button" className="sb-btn sb-btn--ghost" onClick={onCancel} disabled={f.busy}>
            Cancel
          </button>
        )}
      </div>
    </form>
  );
}

// ================================================================== row actions

const DANGER = /(cancel|suspend|retire|withdraw|revoke|reject|archive|delete|remove|close|freeze|incompatible|drop)/;

/** A row's actions as ONE pill (the first primary, or the only action) + kebab for the rest. */
export function RowMenu({ actions, disabled, label = "More actions" }: { actions: RowAction[]; disabled?: boolean; label?: string }) {
  if (actions.length === 0) return null;
  const primary = actions.find((a) => a.variant === "primary") ?? (actions.length === 1 ? actions[0] : undefined);
  const rest = actions.filter((a) => a !== primary);
  const menu: MenuItem[] = rest.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) && !/unfreeze|reactivate/.test(a.id) }));
  return (
    <span className="ac-rowmenu">
      <RowActions
        primary={primary ? { label: primary.label, onClick: primary.onClick, disabled, ghost: primary.variant !== "primary", "data-action": primary.id } : undefined}
        menu={menu}
        label={label}
      />
    </span>
  );
}

// ================================================================== admin frame

/** Header tab link whose active state follows the current tab (not the URL prefix). */
function useTabLink(activeTo: string, keyOf: Record<string, string>): ComponentType<NavLinkLikeProps> {
  return useMemo(
    () =>
      function TabLink({ to, className, children }: NavLinkLikeProps) {
        const on = to === activeTo;
        const cls = typeof className === "function" ? className({ isActive: on }) : cx(className, on && "on");
        return (
          <NavLink to={to} end className={cls} aria-current={on ? "page" : undefined} data-tab-link={keyOf[to]}>
            {children}
          </NavLink>
        );
      },
    [activeTo, keyOf],
  );
}

export interface FrameTab {
  key: string;
  label: string;
  /** Explicit route (another area); defaults to `${base}/${key}` (or `base` for the default tab). */
  to?: string;
  count?: number;
}

/**
 * Admin frame of `/academics` and `/academic-delivery`: administrator gate, `AreaHeader` (h1 +
 * live summary, ≤5 tabs + More, institution picker when there are several), then the tab body.
 */
export function AcademicFrame({
  title,
  base,
  tabs,
  current,
  summary,
  back,
  actions,
  children,
}: {
  title: string;
  base: string;
  tabs: FrameTab[];
  current: string;
  summary?: (institution: Institution | null) => ReactNode;
  back?: { label: string; to: string };
  actions?: ReactNode;
  children: (institution: Institution | null) => ReactNode;
}) {
  const { role } = useSession();
  const { list, current: institution, query } = useInstitutions();
  const hrefOf = (t: FrameTab, i: number) => t.to ?? (i === 0 ? base : `${base}/${t.key}`);
  const keyOf = useMemo(() => Object.fromEntries(tabs.map((t, i) => [hrefOf(t, i), t.key])), [tabs, base]); // eslint-disable-line react-hooks/exhaustive-deps
  const activeIndex = Math.max(
    0,
    tabs.findIndex((t) => t.key === current),
  );
  const activeTab = tabs[activeIndex];
  const TabLink = useTabLink(activeTab ? hrefOf(activeTab, activeIndex) : base, keyOf);
  if (role !== "administrator")
    return (
      <BentoGrid data-screen={base.slice(1)}>
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>{title} is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  const areaTabs: AreaTab[] = tabs.map((t, i) => ({ id: t.key, label: t.label, to: hrefOf(t, i), count: t.count }));
  const line = institution ? summary?.(institution) : query.isLoading ? "Loading…" : "No institution registered yet — register one in Campus → Setup.";
  return (
    <BentoGrid data-screen={base.slice(1)} data-tab={current}>
      <AreaHeader
        title={title}
        back={back}
        summary={line ? <span data-current-institution={institution?.id}>{line}</span> : undefined}
        tabs={areaTabs}
        activeTab={current}
        linkComponent={TabLink}
        tabsLabel={`${title} sections`}
        actions={
          list.length > 1 && institution ? (
            <>
              <div className="ac-inst-pick">
                <FormSelect label="Institution" hideLabel name="institution" value={institution.id} options={list.map((i) => ({ value: i.id, label: i.name }))} onChange={(e) => selectInstitution(e.target.value)} />
              </div>
              {actions}
            </>
          ) : (
            actions
          )
        }
      />
      {children(institution)}
    </BentoGrid>
  );
}

// ================================================================== shared data helpers

export interface TermOption {
  value: string;
  label: string;
  name: string;
  code: string;
  startsOn?: string;
  endsOn?: string;
}

/** Published terms of the institution's calendars (query keys under the `institution` prefix). */
export function useTerms(institutionId: string | null | undefined): TermOption[] {
  const inst = useInstitutionApi();
  const cals = useQuery({ queryKey: ["institution", "r3", institutionId ?? "-", "calendars"], queryFn: () => inst.listCalendars(institutionId!), enabled: !!institutionId });
  const versions = useQueries({ queries: (cals.data ?? []).map((c) => ({ queryKey: ["institution", "r3", c.id, "calendar-versions"], queryFn: () => inst.listVersions(c.id) })) });
  return versions
    .flatMap((q) => q.data ?? [])
    .filter((v) => v.status === "published")
    .flatMap((v) => v.terms.map((t) => ({ value: t.id, label: t.name, name: t.name, code: t.code, startsOn: t.startsOn, endsOn: t.endsOn })));
}

/** Course tones cycle (prototype class / course cards). */
const TONES: Tone[] = ["blue", "violet", "orange", "teal", "rose", "green", "amber"];
export function toneOf(key: string, palette: Tone[] = TONES): Tone {
  let h = 0;
  for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) >>> 0;
  return palette[h % palette.length] ?? "blue";
}

/** "CS101-T1" → "CS101" (course code without the term suffix). */
export const courseCode = (offeringCode: string, code?: string | null) => code || offeringCode.replace(/-T\d+$/i, "");

/** "A" → "Section A". */
export const sectionLabel = (code: string) => (/^sec/i.test(code) ? code : `Section ${code}`);

export const DAY_SHORT = ["", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;
export const DAY_LONG = ["", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"] as const;

/** ISO weekday (1 = Monday) of a date in Asia/Kolkata. */
export function isoWeekday(d: Date = new Date()): number {
  const name = new Intl.DateTimeFormat("en-GB", { weekday: "short", timeZone: "Asia/Kolkata" }).format(d);
  return Math.max(1, ["", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].indexOf(name));
}

export const hhmm = (t: string) => t.slice(0, 5);
export const minutesOf = (t: string) => {
  const m = /^(\d{1,2}):(\d{2})/.exec(t);
  return m ? Number(m[1]) * 60 + Number(m[2]) : 0;
};

export interface SlotLike {
  id: string;
  dayOfWeek: number;
  startsAt: string;
  endsAt: string;
  offeringCode: string;
  sectionCode: string;
  title: string;
  roomLabel?: string | null;
}

/** Week columns (Mon–Fri, plus Sat / Sun when a slot falls there) and `WeekGrid` slots + hour range. */
export function weekOf<T extends SlotLike>(slots: T[]) {
  const days = [1, 2, 3, 4, 5, 6, 7].filter((d) => d <= 6 || slots.some((s) => s.dayOfWeek === d));
  const grid: WeekSlot[] = slots.map((s) => ({
    id: s.id,
    day: days.indexOf(s.dayOfWeek),
    start: hhmm(s.startsAt),
    end: hhmm(s.endsAt),
    title: `${courseCode(s.offeringCode)} ${s.title}`,
    meta: `${hhmm(s.startsAt)}–${hhmm(s.endsAt)}${s.roomLabel ? ` · ${s.roomLabel}` : ""}`,
    tone: toneOf(s.offeringCode),
  }));
  const first = Math.min(9 * 60, ...slots.map((s) => minutesOf(s.startsAt)));
  const last = Math.max(17 * 60, ...slots.map((s) => minutesOf(s.endsAt)));
  const today = isoWeekday();
  return { days: days.map((d) => DAY_SHORT[d] as string), dayNos: days, grid, startHour: Math.floor(first / 60), endHour: Math.ceil(last / 60), todayIndex: days.indexOf(today) >= 0 ? days.indexOf(today) : undefined };
}

/** Sum of slot durations in hours ("7.5"). */
export function slotHours(slots: SlotLike[]): number {
  return slots.reduce((n, s) => n + Math.max(0, minutesOf(s.endsAt) - minutesOf(s.startsAt)), 0) / 60;
}

/** Next upcoming slot from now (same week, wraps to next week). */
export function nextSlot<T extends SlotLike>(slots: T[], now = new Date()): T | undefined {
  const today = isoWeekday(now);
  const parts = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit", hour12: false, timeZone: "Asia/Kolkata" }).format(now);
  const nowKey = today * 1440 + minutesOf(parts);
  const keyed = slots.map((s) => ({ s, k: s.dayOfWeek * 1440 + minutesOf(s.startsAt) })).sort((a, b) => a.k - b.k);
  return (keyed.find((x) => x.k >= nowKey) ?? keyed[0])?.s;
}

/** "Today, 10:30" / "Thu, 10:30" for a slot. */
export function slotWhen(s: SlotLike, now = new Date()): string {
  return `${s.dayOfWeek === isoWeekday(now) ? "Today" : DAY_SHORT[s.dayOfWeek]}, ${hhmm(s.startsAt)}${s.roomLabel ? ` · ${s.roomLabel}` : ""}`;
}

/** Attendance percent → badge kind (prototype: ≥ 85 green, ≥ 75 amber, below → red). */
export function attendanceKind(p: number, threshold = 75): "done" | "review" | "attention" {
  return p < threshold ? "attention" : p < 85 ? "review" : "done";
}

/** "88%" (no decimals unless needed). */
export const pctText = (p: number) => `${Math.round(p * 10) / 10}%`;

/** Drop-in for `institution/forms` `Actions`: one pill + kebab. */
export const Actions = RowMenu;
export { fmtDate, NoInstitution, REASON, todayIso } from "../institution/forms";

/** Router link for `AreaHeader.back` (never "active"). */
export function PlainLink({ to, className, children }: NavLinkLikeProps) {
  return (
    <Link to={to} className={typeof className === "function" ? className({ isActive: false }) : className}>
      {children}
    </Link>
  );
}
