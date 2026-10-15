/**
 * Slice **3B** web components (registration, progression and credits). The export names are
 * imported by the route frames (docs/batch3-contracts.md §10).
 *
 * - `RegistrationAdminTab` — Admin → Students → Registration: windows of the selected
 *   institution (schedule, open, close, cancel, finalise).
 * - `ProgressionAdminTab` — Admin → Students → Progression: progression policy versions.
 * - `Student360Registration` / `Student360Progression` — Student 360 tabs.
 * - `StudentRegistrationScreen` — student-facing `/registration` (self-registration, add/drop).
 */
import { Award, CalendarCheck, GraduationCap, Layers, Plus } from "lucide-react";
import { AreaHeader, BentoTile, Button, Checkbox, CheckboxList, DataTable, EmptyState, FormSelect, KpiTile, StatusBadge, humanize } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useInstitutionApi } from "../../institution/hooks";
import { Actions, creditsText, fmtDate, FormDialog, NoInstitution, REASON, todayIso, useAsk, type RowAction } from "../ui";
// The record command runner also refreshes the Student 360 header/overview (3A queries) after 3B commands.
import { useRecordCommand } from "../record/hooks";
import type { Institution } from "../../institution/types";
import { rkeys, useRegistrationApi } from "./hooks";
import type { HoldSource, LineInput, ProgressionOutcome, Registration, RegistrationLineKind, RegistrationOptions } from "./types";

export { RegistrationApiProvider } from "./hooks";

const credits = creditsText;

/** Finance evidence (`kind` is camelCase on the wire) in words. */
const FINANCE_LABEL: Record<string, string> = {
  cleared: "Cleared",
  outstanding: "Dues outstanding",
  pending: "Pending",
  notApplicable: "No obligation configured",
};

const OUTCOMES: ProgressionOutcome[] = ["progressed", "conditionally_progressed", "probation", "detained", "repeat", "qualification_eligible"];
const HOLD_SOURCES: HoldSource[] = ["academic", "administrative", "discipline", "documents", "finance"];

/** Published terms of the institution's calendars (window scheduling, decisions). */
function useTerms(institutionId: string | null | undefined) {
  const inst = useInstitutionApi();
  const cals = useQuery({ queryKey: rkeys.scoped(institutionId ?? "-", "calendars"), queryFn: () => inst.listCalendars(institutionId!), enabled: !!institutionId });
  const versions = useQueries({
    queries: (cals.data ?? []).map((c) => ({ queryKey: rkeys.scoped(c.id, "calendar-versions"), queryFn: () => inst.listVersions(c.id) })),
  });
  return versions
    .flatMap((q) => q.data ?? [])
    .filter((v) => v.status === "published")
    .flatMap((v) => v.terms.map((t) => ({ value: t.id, label: `${t.name} · ${fmtDate(t.startsOn)} – ${fmtDate(t.endsOn)}` })));
}

// ================================================================== admin: windows

export function RegistrationAdminTab({ institution }: { institution: Institution | null }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const [adding, setAdding] = useState(false);
  const terms = useTerms(institution?.id);
  const q = useQuery({ queryKey: rkeys.scoped(institution?.id ?? "-", "windows"), queryFn: () => api.listWindows(institution!.id), enabled: !!institution });
  if (!institution)
    return (
      <BentoTile span={12}>
        <NoInstitution />
      </BentoTile>
    );
  return (
    <>
      <BentoTile
        span={12}
        title="Registration windows"
        data-slice="3B"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-window>
            Schedule window
          </Button>
        }
      >
        <DataTable
          caption="Registration windows"
          rows={q.data ?? []}
          rowKey={(w) => w.id}
          empty={
            <EmptyState icon={CalendarCheck} title="No registration windows">
              Schedule one per term; it opens on its first day.
            </EmptyState>
          }
          columns={[
            { key: "name", header: "Window", render: (w) => <span data-window={w.id}>{w.name}</span> },
            { key: "dates", header: "Dates", render: (w) => `${fmtDate(w.opensOn)} – ${fmtDate(w.closesOn)}`, muted: true },
            { key: "credits", header: "Credits", render: (w) => `${w.minCredits ?? "—"} – ${w.maxCredits ?? "—"}`, muted: true },
            { key: "status", header: "Status", render: (w) => <StatusBadge status={w.status} /> },
            {
              key: "a",
              header: "",
              render: (w) => {
                const actions: RowAction[] = [];
                if (w.status === "scheduled") {
                  actions.push({ id: "window-open", label: "Open", variant: "primary", onClick: () => void run(() => api.windowCommand(w.id, w.version, "open"), `${w.name}: open`) });
                  actions.push({
                    id: "window-cancel",
                    label: "Cancel",
                    onClick: () => ask({ title: "Cancel window", name: "window-cancel", fields: [REASON], submitLabel: "Cancel window", onSubmit: (v) => submit(() => api.windowCommand(w.id, w.version, "cancel", v.reason), "Window cancelled") }),
                  });
                }
                if (w.status === "open") actions.push({ id: "window-close", label: "Close", onClick: () => void run(() => api.windowCommand(w.id, w.version, "close"), `${w.name}: closed`) });
                if (w.status === "closed")
                  actions.push({
                    id: "window-finalise",
                    label: "Finalise registrations",
                    variant: "primary",
                    onClick: () => void run(() => api.finaliseWindow(w.id, w.version), (f) => `${f.finalisedRegistrationIds.length} registration(s) finalised`),
                  });
                return <Actions disabled={busy} actions={actions} />;
              },
            },
          ]}
        />
      </BentoTile>
      <FormDialog open={adding} onClose={() => setAdding(false)} eyebrow="Students" title="Schedule registration window"
          name="new-window"
          submitLabel="Schedule"
          fields={[
            { name: "academicTermId", label: "Term", type: "select", required: true, options: [{ value: "", label: "Choose a term" }, ...terms], wide: true },
            { name: "name", label: "Name", placeholder: "Term 1 registration" },
            { name: "opensOn", label: "Opens on", type: "date", initial: todayIso() },
            { name: "closesOn", label: "Closes on", type: "date" },
            { name: "minCredits", label: "Minimum credits", type: "number" },
            { name: "maxCredits", label: "Maximum credits", type: "number" },
          ]}
          onSubmit={async (v) => {
            await submit(
              () =>
                api.createWindow(institution.id, {
                  academicTermId: v.academicTermId!,
                  name: v.name?.trim() || null,
                  opensOn: v.opensOn || null,
                  closesOn: v.closesOn || null,
                  minCredits: v.minCredits ? Number(v.minCredits) : null,
                  maxCredits: v.maxCredits ? Number(v.maxCredits) : null,
                }),
              "Window scheduled",
            );
            setAdding(false);
          }}
        />
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: policies

export function ProgressionAdminTab({ institution }: { institution: Institution | null }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const [adding, setAdding] = useState(false);
  const q = useQuery({ queryKey: rkeys.scoped(institution?.id ?? "-", "policies"), queryFn: () => api.listPolicies(institution!.id), enabled: !!institution });
  if (!institution)
    return (
      <BentoTile span={12}>
        <NoInstitution />
      </BentoTile>
    );
  const pct = (r: number) => `${Math.round(r * 100)}%`;
  return (
    <>
      <BentoTile
        span={12}
        title="Progression policies"
        data-slice="3B"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-progression-policy>
            Define policy
          </Button>
        }
      >
        <DataTable
          caption="Progression policies"
          rows={q.data ?? []}
          rowKey={(p) => p.id}
          empty={
            <EmptyState icon={GraduationCap} title="No progression policy">
              Without one, every decision&apos;s outcome is chosen by hand.
            </EmptyState>
          }
          columns={[
            {
              key: "code",
              header: "Policy",
              render: (p) => (
                <span data-policy={p.code}>
                  {p.name} · {p.code} v{p.versionNo}
                </span>
              ),
            },
            { key: "eff", header: "Effective from", render: (p) => fmtDate(p.effectiveFrom), muted: true },
            {
              key: "rules",
              header: "Thresholds",
              render: (p) => `progress ≥ ${pct(p.rules.progressMinRatio)} · conditional ≥ ${pct(p.rules.conditionalMinRatio)} · probation ≥ ${pct(p.rules.probationMinRatio)}`,
              muted: true,
            },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "a",
              header: "",
              render: (p) => {
                const actions: RowAction[] = [];
                if (p.status === "draft") actions.push({ id: "policy-submit", label: "Make effective", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "submit"), `${p.code}: submitted`) });
                if (p.status === "effective") {
                  actions.push({ id: "policy-revise", label: "New version", onClick: () => void run(() => api.revisePolicy(p.id), (n) => `Draft v${n.versionNo} created`) });
                  actions.push({
                    id: "policy-retire",
                    label: "Retire",
                    onClick: () => ask({ title: "Retire policy", name: "policy-retire", fields: [REASON], submitLabel: "Retire", onSubmit: (v) => submit(() => api.policyCommand(p.id, p.version, "retire", v.reason), "Policy retired") }),
                  });
                }
                return <Actions disabled={busy} actions={actions} />;
              },
            },
          ]}
        />
      </BentoTile>
      <FormDialog open={adding} onClose={() => setAdding(false)} eyebrow="Students" title="Define progression policy"
          name="new-progression-policy"
          submitLabel="Create draft"
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "UG" },
            { name: "name", label: "Name", required: true, placeholder: "UG progression rules" },
            { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
            { name: "progress", label: "Progress at earned/attempted ≥ (%)", type: "number", required: true, initial: "100" },
            { name: "conditional", label: "Conditional promotion ≥ (%)", type: "number", required: true, initial: "75" },
            { name: "probation", label: "Probation ≥ (%), below → detained", type: "number", required: true, initial: "50" },
            {
              name: "repeat",
              label: "Detention repeats a",
              type: "select",
              options: [
                { value: "term", label: "Term" },
                { value: "year", label: "Year" },
              ],
              initial: "term",
            },
          ]}
          onSubmit={async (v) => {
            await submit(
              () =>
                api.createPolicy(institution.id, {
                  code: v.code!.trim(),
                  name: v.name!.trim(),
                  effectiveFrom: v.effectiveFrom!,
                  rules: {
                    progressMinRatio: Number(v.progress) / 100,
                    conditionalMinRatio: Number(v.conditional) / 100,
                    probationMinRatio: Number(v.probation) / 100,
                    detentionRepeatUnit: v.repeat || "term",
                  },
                }),
              "Policy drafted",
            );
            setAdding(false);
          }}
        />
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== shared: registration card

/** Components that can still be added to `r` (not earned, not already on a live line). */
function addableComponents(r: Registration, options: RegistrationOptions | undefined) {
  const taken = new Set(r.lines.filter((l) => l.status === "selected" || l.status === "registered").map((l) => l.curriculumComponentId));
  return (options?.components ?? []).filter((c) => !c.earned && c.suggestedKind && !taken.has(c.curriculumComponentId));
}

/** The "Add component" dialog (add/drop): picks one of the addable components of `r`. */
function askAddLine(
  r: Registration,
  options: RegistrationOptions | undefined,
  ask: ReturnType<typeof useAsk>["ask"],
  add: (line: LineInput) => Promise<unknown>,
) {
  const choices = addableComponents(r, options);
  ask({
    title: `Add a component to the term ${r.termNo} registration`,
    name: "line-add",
    submitLabel: "Add component",
    fields: [
      {
        name: "curriculumComponentId",
        label: "Component",
        type: "select",
        required: true,
        initial: choices[0]?.curriculumComponentId ?? "",
        options: choices.map((c) => ({
          value: c.curriculumComponentId,
          label: `${c.slotCode} · ${c.title} · ${credits(c.credits)}${c.suggestedKind && c.suggestedKind !== "regular" ? ` · ${humanize(c.suggestedKind)}` : ""}`,
        })),
        wide: true,
      },
    ],
    onSubmit: (v) => {
      const c = choices.find((x) => x.curriculumComponentId === v.curriculumComponentId);
      return add({ curriculumComponentId: v.curriculumComponentId!, kind: c?.suggestedKind ?? "regular" });
    },
  });
}

function RegistrationCard({
  r,
  busy,
  canAdd,
  onSubmit,
  onAdd,
  onDrop,
  onCancel,
  onFinalise,
}: {
  r: Registration;
  busy: boolean;
  /** Whether a component is left to add (the add action is hidden otherwise). */
  canAdd?: boolean;
  onSubmit?: () => void;
  onAdd?: () => void;
  onDrop?: (lineId: string) => void;
  onCancel?: () => void;
  onFinalise?: () => void;
}) {
  const editable = r.status === "draft" || r.status === "returned" || r.status === "registered";
  const actions: RowAction[] = [];
  if (onSubmit && (r.status === "draft" || r.status === "returned")) actions.push({ id: "registration-submit", label: "Submit", variant: "primary", onClick: onSubmit });
  if (onFinalise && r.status === "registered") actions.push({ id: "registration-finalise", label: "Finalise", variant: "primary", onClick: onFinalise });
  if (onCancel && !["finalised", "cancelled", "archived"].includes(r.status)) actions.push({ id: "registration-cancel", label: "Cancel registration", onClick: onCancel });
  const live = r.lines.filter((l) => l.status !== "dropped");
  return (
    <BentoTile span={12} title={`Term ${r.termNo} registration`} action={<StatusBadge status={r.status} />}>
      <div data-registration={r.id}>
        <p className="st-meta" style={{ marginBottom: 12 }}>
          {credits(r.totalCredits)} · {r.origin === "self" ? "registered by the student" : "registered by staff"}
          {r.statusReason ? ` · ${r.statusReason}` : ""}
        </p>
        <DataTable
          caption="Registered components"
          rows={r.lines}
          rowKey={(l) => l.id}
          empty={<EmptyState title="No components selected" compact />}
          columns={[
            {
              key: "slot",
              header: "Component",
              render: (l) => (
                <span>
                  <b>{`${l.slotCode} · ${l.title}`}</b>
                </span>
              ),
            },
            { key: "kind", header: "Type", render: (l) => humanize(l.kind), muted: true },
            { key: "cr", header: "Credits", render: (l) => credits(l.credits), muted: true },
            { key: "st", header: "Status", render: (l) => <StatusBadge status={l.status} /> },
            {
              key: "a",
              header: "",
              render: (l) =>
                onDrop && editable && (l.status === "selected" || l.status === "registered") ? <Actions disabled={busy} actions={[{ id: "line-drop", label: "Drop", onClick: () => onDrop(l.id) }]} /> : null,
            },
          ]}
        />
        <div className="st-total">
          <span className="st-meta">{`${live.length} ${live.length === 1 ? "component" : "components"}`}</span>
          <span style={{ display: "inline-flex", gap: 10, alignItems: "center", marginLeft: "auto" }}>
            {onAdd && canAdd && editable && (
              <button type="button" className="sb-btn sb-btn--ghost" disabled={busy} onClick={onAdd} data-action="line-add">
                Add component
              </button>
            )}
            <Actions disabled={busy} actions={actions} label="Registration actions" />
          </span>
        </div>
      </div>
    </BentoTile>
  );
}

/** Component picker for a new registration (open windows + curriculum components). */
function NewRegistration({
  options,
  busy,
  onCreate,
}: {
  options: RegistrationOptions;
  busy: boolean;
  onCreate: (termId: string, lines: { curriculumComponentId: string; kind: RegistrationLineKind }[]) => Promise<unknown>;
}) {
  const open = options.windows.filter((w) => w.status === "open");
  const [term, setTerm] = useState(open[0]?.academicTermId ?? "");
  const [picked, setPicked] = useState<Record<string, boolean>>({});
  const choices = options.components.filter((c) => !c.earned && c.suggestedKind);
  if (open.length === 0) return <EmptyState icon={CalendarCheck} title="No registration window is open" />;
  if (options.activeHolds.length > 0) return <EmptyState title="Registration is on hold">{options.activeHolds.map((h) => h.reason).join(" · ")}</EmptyState>;
  const selected = choices.filter((c) => picked[c.curriculumComponentId]);
  const total = selected.reduce((a, c) => a + c.credits, 0);
  const win = open.find((w) => w.academicTermId === term);
  return (
    <div data-form="new-registration" style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      {open.length > 1 && <FormSelect label="Window" value={term} onChange={(e) => setTerm(e.target.value)} options={open.map((w) => ({ value: w.academicTermId, label: `${w.name} · closes ${fmtDate(w.closesOn)}` }))} />}
      {open.length === 1 && win && <p className="st-meta">{`${win.name} · closes ${fmtDate(win.closesOn)}${win.minCredits || win.maxCredits ? ` · ${win.minCredits ?? 0}–${win.maxCredits ?? "any"} credits` : ""}`}</p>}
      <CheckboxList label="Components">
        {choices.map((c) => (
          <Checkbox
            key={c.curriculumComponentId}
            data-component={c.slotCode}
            checked={!!picked[c.curriculumComponentId]}
            onChange={(e) => setPicked((p) => ({ ...p, [c.curriculumComponentId]: e.target.checked }))}
            label={c.title}
            description={[c.slotCode, c.suggestedKind && c.suggestedKind !== "regular" ? humanize(c.suggestedKind) : null].filter(Boolean).join(" · ")}
            note={credits(c.credits)}
          />
        ))}
      </CheckboxList>
      <div className="st-total">
        <b>{`${credits(total)} selected`}</b>
        <button
          type="button"
          className="sb-btn"
          disabled={busy || !term || total === 0}
          data-action="registration-create"
          onClick={() =>
            void onCreate(
              term,
              selected.map((c) => ({ curriculumComponentId: c.curriculumComponentId, kind: c.suggestedKind ?? "regular" })),
            ).then(() => setPicked({}))
          }
        >
          Start registration
        </button>
      </div>
    </div>
  );
}

// ================================================================== Student 360: registration

export function Student360Registration({ studentId }: { studentId: string }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const regs = useQuery({ queryKey: rkeys.scoped(studentId, "registrations"), queryFn: () => api.listRegistrations(studentId) });
  const opts = useQuery({ queryKey: rkeys.scoped(studentId, "options"), queryFn: () => api.options(studentId) });
  const holds = useQuery({ queryKey: rkeys.scoped(studentId, "holds"), queryFn: () => api.listHolds(studentId) });
  const placeHold = () =>
    ask({
      title: "Place registration hold",
      name: "hold-place",
      fields: [{ name: "source", label: "Source", type: "select", options: HOLD_SOURCES.map((s) => ({ value: s, label: humanize(s) })), initial: "administrative" }, REASON],
      submitLabel: "Place hold",
      onSubmit: (v) => submit(() => api.placeHold(studentId, { source: (v.source || "administrative") as HoldSource, reason: v.reason! }), "Hold placed"),
    });
  return (
    <>
      <BentoTile
        span={6}
        title="Holds"
        data-slice="3B"
        action={
          <Button size="sm" data-action="hold-place" onClick={placeHold}>
            Place hold
          </Button>
        }
      >
        <p className="is-muted" data-finance={holds.data?.financeEvidence.kind}>
          Finance: {holds.data ? (FINANCE_LABEL[holds.data.financeEvidence.kind] ?? humanize(holds.data.financeEvidence.kind)) : "—"}
        </p>
        <DataTable
          caption="Holds"
          rows={holds.data?.items ?? []}
          rowKey={(h) => h.id}
          empty={<EmptyState title="No holds" />}
          columns={[
            { key: "src", header: "Source", render: (h) => humanize(h.source) },
            { key: "why", header: "Reason", render: (h) => h.reason, muted: true },
            { key: "st", header: "Status", render: (h) => <StatusBadge status={h.status} /> },
            {
              key: "a",
              header: "",
              render: (h) =>
                h.status === "active" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      {
                        id: "hold-release",
                        label: "Release",
                        onClick: () => ask({ title: "Release hold", name: "hold-release", fields: [REASON], submitLabel: "Release", onSubmit: (v) => submit(() => api.releaseHold(h.id, h.version, v.reason!), "Hold released") }),
                      },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="New registration">
        {opts.data ? (
          <NewRegistration options={opts.data} busy={busy} onCreate={(t, lines) => run(() => api.createRegistration(studentId, { academicTermId: t, lines }), "Registration started")} />
        ) : (
          <p className="is-muted">Loading…</p>
        )}
      </BentoTile>
      {(regs.data ?? []).map((r) => (
        <RegistrationCard
          key={r.id}
          r={r}
          busy={busy}
          canAdd={addableComponents(r, opts.data).length > 0}
          onAdd={() => askAddLine(r, opts.data, ask, (line) => submit(() => api.addLine(r.id, r.version, line), "Component added"))}
          onSubmit={() => void run(() => api.registrationCommand(r.id, r.version, "submit"), (x) => `Registration ${humanize(x.status).toLowerCase()}`)}
          onDrop={(lineId) => void run(() => api.dropLine(r.id, r.version, lineId), "Component dropped")}
          onFinalise={() => void run(() => api.registrationCommand(r.id, r.version, "finalise"), "Registration finalised")}
          onCancel={() =>
            ask({ title: "Cancel registration", name: "registration-cancel", fields: [REASON], submitLabel: "Cancel registration", onSubmit: (v) => submit(() => api.registrationCommand(r.id, r.version, "cancel", v.reason), "Registration cancelled") })
          }
        />
      ))}
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== Student 360: progression

export function Student360Progression({ studentId }: { studentId: string }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const sum = useQuery({ queryKey: rkeys.scoped(studentId, "progression"), queryFn: () => api.summary(studentId) });
  const decisions = useQuery({ queryKey: rkeys.scoped(studentId, "decisions"), queryFn: () => api.listDecisions(studentId) });
  const ledger = useQuery({ queryKey: rkeys.scoped(studentId, "ledger"), queryFn: () => api.ledger(studentId) });
  const breaks = useQuery({ queryKey: rkeys.scoped(studentId, "breaks"), queryFn: () => api.listBreaks(studentId) });
  const transfers = useQuery({ queryKey: rkeys.scoped(studentId, "transfers"), queryFn: () => api.listTransfers(studentId) });
  const abc = useQuery({ queryKey: rkeys.scoped(studentId, "abc"), queryFn: () => api.listAbcSyncs(studentId) });
  const opts = useQuery({ queryKey: rkeys.scoped(studentId, "options"), queryFn: () => api.options(studentId) });
  const terms = useTerms(opts.data?.institutionId);
  const s = sum.data;
  const elig = s?.eligibility;
  const lastAbc = abc.data?.[0];
  const recordDecision = () =>
    ask({
      title: "Record progression decision",
      name: "decision-new",
      fields: [
        { name: "academicTermId", label: "Term", type: "select", required: true, options: [{ value: "", label: "Choose a term" }, ...terms], wide: true },
        { name: "creditsAttempted", label: "Credits attempted", type: "number" },
        { name: "creditsEarned", label: "Credits earned", type: "number", required: true },
        { name: "outcome", label: "Outcome", type: "select", options: [{ value: "", label: "Policy recommendation" }, ...OUTCOMES.map((o) => ({ value: o, label: humanize(o) }))] },
        { name: "evidenceNote", label: "Evidence note", wide: true },
      ],
      submitLabel: "Save draft",
      onSubmit: (v) =>
        submit(
          () =>
            api.createDecision(studentId, {
              academicTermId: v.academicTermId!,
              creditsAttempted: v.creditsAttempted ? Number(v.creditsAttempted) : null,
              creditsEarned: Number(v.creditsEarned),
              outcome: v.outcome ? (v.outcome as ProgressionOutcome) : null,
              evidenceNote: v.evidenceNote || null,
            }),
          "Decision drafted",
        ),
    });
  const requestBreak = () =>
    ask({
      title: "Request academic break",
      name: "break-request",
      fields: [REASON, { name: "startsOn", label: "Starts on", type: "date", required: true, initial: todayIso() }, { name: "expectedReturnOn", label: "Expected return", type: "date", required: true }],
      submitLabel: "Request",
      onSubmit: (v) => submit(() => api.requestBreak(studentId, { reason: v.reason!, startsOn: v.startsOn!, expectedReturnOn: v.expectedReturnOn! }), (b) => `Break ${humanize(b.status).toLowerCase()}`),
    });
  return (
    <>
      <KpiTile span={3} icon={GraduationCap} tone="violet" label="Standing" value={s ? humanize(s.standing) : "—"} />
      <KpiTile span={3} icon={CalendarCheck} tone="blue" label="Current term" value={String(s?.currentTermNo ?? "—")} />
      <KpiTile span={3} icon={Layers} tone="green" label="Credits" value={s ? String(s.totalCredits) : "—"} />
      <KpiTile span={3} icon={Award} tone="amber" label="Qualification" value={elig ? (elig.eligible ? "Eligible" : "Pending") : "Not evaluated"} />
      <BentoTile
        span={12}
        title="Progression decisions"
        data-slice="3B"
        action={
          <span style={{ display: "inline-flex", gap: 8 }}>
            {(breaks.data ?? []).length === 0 && (
              <Button size="sm" variant="ghost" data-action="break-request" onClick={requestBreak}>
                Request break
              </Button>
            )}
            <Button size="sm" data-action="decision-new" onClick={recordDecision}>
              Record decision
            </Button>
          </span>
        }
      >
        <DataTable
          caption="Progression decisions"
          rows={decisions.data ?? []}
          rowKey={(d) => d.id}
          empty={<EmptyState title="No decisions yet">Results are entered by hand until Exams publishes them.</EmptyState>}
          columns={[
            { key: "term", header: "Term", render: (d) => `Term ${d.termNo}` },
            {
              key: "outcome",
              header: "Outcome",
              render: (d) => (
                <span data-outcome={d.outcome}>
                  {humanize(d.outcome)}
                  {d.recommendedOutcome && d.recommendedOutcome !== d.outcome ? ` (policy: ${humanize(d.recommendedOutcome)})` : ""}
                </span>
              ),
            },
            { key: "cr", header: "Earned", render: (d) => `${d.creditsEarned}${d.creditsAttempted ? ` / ${d.creditsAttempted}` : ""}`, muted: true },
            { key: "st", header: "Status", render: (d) => <StatusBadge status={d.status} /> },
            {
              key: "a",
              header: "",
              render: (d) =>
                d.status === "draft" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      { id: "decision-submit", label: "Make effective", variant: "primary", onClick: () => void run(() => api.decisionCommand(d.id, d.version, "submit"), (x) => `Decision ${humanize(x.status).toLowerCase()}`) },
                      { id: "decision-discard", label: "Discard", onClick: () => void run(() => api.decisionCommand(d.id, d.version, "discard"), "Draft discarded") },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      {(s?.pendingConditions.length ?? 0) > 0 && (
        <BentoTile span={6} title="Promotion conditions">
          <DataTable
            caption="Pending conditions"
            rows={s?.pendingConditions ?? []}
            rowKey={(c) => c.id}
            columns={[
              { key: "d", header: "Condition", render: (c) => c.description },
              { key: "due", header: "Due", render: (c) => fmtDate(c.dueOn), muted: true },
              {
                key: "a",
                header: "",
                render: (c) => (
                  <Actions
                    disabled={busy}
                    actions={[
                      { id: "condition-fulfil", label: "Fulfilled", onClick: () => void run(() => api.conditionCommand(c.id, c.version, "fulfil"), "Condition fulfilled") },
                      {
                        id: "condition-fail",
                        label: "Failed",
                        onClick: () => ask({ title: "Condition failed", name: "condition-fail", fields: [REASON], submitLabel: "Record failure", onSubmit: (v) => submit(() => api.conditionCommand(c.id, c.version, "fail", v.reason), "Condition failed") }),
                      },
                    ]}
                  />
                ),
              },
            ]}
          />
        </BentoTile>
      )}
      {(s?.probationCases ?? [])
        .filter((p) => p.status === "active")
        .map((p) => (
          <BentoTile key={p.id} span={6} title="Academic probation">
            <div data-probation={p.id}>
              <p>Review due {fmtDate(p.reviewDueOn)}</p>
              <Actions
                disabled={busy}
                actions={[
                  { id: "probation-clear", label: "Clear", onClick: () => ask({ title: "Clear probation", name: "probation-clear", fields: [REASON], submitLabel: "Clear", onSubmit: (v) => submit(() => api.probationCommand(p.id, p.version, "clear", v.reason!), "Probation cleared") }) },
                  {
                    id: "probation-escalate",
                    label: "Escalate",
                    onClick: () => ask({ title: "Escalate probation", name: "probation-escalate", fields: [REASON], submitLabel: "Escalate", onSubmit: (v) => submit(() => api.probationCommand(p.id, p.version, "escalate", v.reason!), "Probation escalated") }),
                  },
                ]}
              />
            </div>
          </BentoTile>
        ))}
      <BentoTile
        span={6}
        title="Credit ledger"
        action={
          <Button size="sm" data-action="abc-sync" disabled={busy} onClick={() => void run(() => api.syncAbc(studentId), (a) => (a.status === "acknowledged" ? "Synchronised with ABC" : `ABC: ${a.failureReason ?? a.status}`))}>
            Sync to ABC
          </Button>
        }
      >
        <DataTable
          caption="Credit ledger"
          rows={ledger.data?.entries ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState title="No credits recorded" />}
          columns={[
            { key: "k", header: "Entry", render: (e) => humanize(e.kind) },
            { key: "c", header: "Credits", render: (e) => (e.kind === "revoked" || e.kind === "expired" ? `−${e.credits}` : `${e.credits}`) },
            { key: "src", header: "Source", render: (e) => humanize(e.sourceType), muted: true },
            { key: "at", header: "Recorded", render: (e) => fmtDate(e.recordedAt.slice(0, 10)), muted: true },
          ]}
        />
        {lastAbc && (
          <p className="is-muted">
            Last ABC sync: {humanize(lastAbc.status)}
            {lastAbc.failureReason ? ` (${lastAbc.failureReason})` : ""}
          </p>
        )}
      </BentoTile>
      <BentoTile
        span={6}
        title="Qualification eligibility"
        action={
          <Button size="sm" data-action="eligibility-evaluate" disabled={busy} onClick={() => void run(() => api.evaluateEligibility(studentId), (e) => (e.eligible ? "Qualification eligible" : "Qualification pending"))}>
            Evaluate
          </Button>
        }
      >
        {elig ? (
          <div data-eligibility={elig.eligible ? "eligible" : "pending"}>
            <p>
              {elig.eligible ? "Eligible" : "Pending"} · {elig.totalCredits} / {elig.requiredCredits ?? "—"} credits
              {elig.outcomes.length ? ` · ${elig.outcomes.map((o) => humanize(o)).join(", ")}` : ""}
            </p>
            {elig.outstanding.length > 0 && (
              <ul className="sb-list">
                {elig.outstanding.map((o) => (
                  <li key={o}>{o}</li>
                ))}
              </ul>
            )}
          </div>
        ) : (
          <EmptyState title="Not evaluated yet" />
        )}
      </BentoTile>
      {(breaks.data ?? []).length > 0 && (
      <BentoTile
        span={6}
        title="Academic breaks"
        action={
          <Button size="sm" data-action="break-request" onClick={requestBreak}>
            Request break
          </Button>
        }
      >
        <DataTable
          caption="Academic breaks"
          rows={breaks.data ?? []}
          rowKey={(b) => b.id}
          empty={<EmptyState title="No academic breaks" />}
          columns={[
            { key: "d", header: "Period", render: (b) => `${fmtDate(b.startsOn)} – ${fmtDate(b.expectedReturnOn)}` },
            { key: "r", header: "Reason", render: (b) => b.reason, muted: true },
            { key: "s", header: "Status", render: (b) => <StatusBadge status={b.status} /> },
            {
              key: "a",
              header: "",
              render: (b) =>
                b.status === "in_effect" ? <Actions disabled={busy} actions={[{ id: "break-rejoin", label: "Rejoin", onClick: () => void run(() => api.rejoin(b.id, b.version, { termNo: b.termNoAtStart ?? null }), "Student rejoined") }]} /> : null,
            },
          ]}
        />
      </BentoTile>
      )}
      {(transfers.data ?? []).length > 0 && (
      <BentoTile span={6} title="Credit transfers">
        <DataTable
          caption="Credit transfers"
          rows={transfers.data ?? []}
          rowKey={(t) => t.id}
          empty={<EmptyState title="No credit transfers" />}
          columns={[
            { key: "src", header: "From", render: (t) => t.sourceInstitution },
            { key: "cr", header: "Credits", render: (t) => `${t.creditsRecognised ?? t.creditsRequested}`, muted: true },
            { key: "s", header: "Status", render: (t) => <StatusBadge status={t.status} /> },
          ]}
        />
      </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== student-facing

/** Student-facing `/registration` (self-registration, add/drop, cancellation). */
export function StudentRegistrationScreen() {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const opts = useQuery({ queryKey: rkeys.scoped("me", "options"), queryFn: () => api.myOptions() });
  const mine = useQuery({ queryKey: rkeys.scoped("me", "registrations"), queryFn: () => api.myRegistrations() });
  const live = (mine.data ?? []).filter((r) => r.status !== "cancelled");
  return (
    <>
      <AreaHeader
        title="Registration"
        data-slice="3B"
        summary={opts.data ? [opts.data.termNo ? `Term ${opts.data.termNo}` : null, humanize(opts.data.standing), live[0] ? `registration ${humanize(live[0].status).toLowerCase()}` : "not registered yet"].filter(Boolean).join(" · ") : "Loading…"}
      />
      {live.length === 0 && (
        <BentoTile span={12} title="Register for the term">
          {opts.data ? (
            <NewRegistration options={opts.data} busy={busy} onCreate={(t, lines) => run(() => api.createMyRegistration({ academicTermId: t, lines }), "Registration started")} />
          ) : (
            <p className="is-muted">Loading…</p>
          )}
        </BentoTile>
      )}
      {(mine.data ?? []).map((r) => (
        <RegistrationCard
          key={r.id}
          r={r}
          busy={busy}
          canAdd={addableComponents(r, opts.data).length > 0}
          onAdd={() => askAddLine(r, opts.data, ask, (line) => submit(() => api.addMyLine(r.id, r.version, line), "Component added"))}
          onSubmit={() => void run(() => api.submitMine(r.id, r.version), (x) => `Registration ${humanize(x.status).toLowerCase()}`)}
          onDrop={(lineId) => void run(() => api.dropMyLine(r.id, r.version, lineId), "Component dropped")}
          onCancel={() =>
            ask({ title: "Cancel my registration", name: "my-registration-cancel", fields: [REASON], submitLabel: "Cancel registration", onSubmit: (v) => submit(() => api.cancelMine(r.id, r.version, v.reason!), "Registration cancelled") })
          }
        />
      ))}
      {prompt}
      {dialog}
    </>
  );
}
