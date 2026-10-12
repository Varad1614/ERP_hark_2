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
import { BentoTile, Button, DataTable, EmptyState, Modal, Select, StatTile } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, NoInstitution, QuickForm, REASON, todayIso, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { rkeys, useRegistrationApi } from "./hooks";
import type { HoldSource, ProgressionOutcome, Registration, RegistrationLineKind, RegistrationOptions } from "./types";

export { RegistrationApiProvider } from "./hooks";

const credits = (n: number | null | undefined) => (n === null || n === undefined ? "—" : `${n} cr`);

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
    .flatMap((v) => v.terms.map((t) => ({ value: t.id, label: `${t.name} (${t.code}) · ${fmtDate(t.startsOn)} – ${fmtDate(t.endsOn)}` })));
}

// ================================================================== admin: windows

export function RegistrationAdminTab({ institution }: { institution: Institution | null }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useCommand();
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
      <Modal open={adding} onClose={() => setAdding(false)} label="Schedule registration window">
        <h2>Schedule registration window</h2>
        <QuickForm
          name="new-window"
          submitLabel="Schedule"
          onCancel={() => setAdding(false)}
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
      </Modal>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: policies

export function ProgressionAdminTab({ institution }: { institution: Institution | null }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useCommand();
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
      <Modal open={adding} onClose={() => setAdding(false)} label="Define progression policy">
        <h2>Define progression policy</h2>
        <QuickForm
          name="new-progression-policy"
          submitLabel="Create draft"
          onCancel={() => setAdding(false)}
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
      </Modal>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== shared: registration card

function RegistrationCard({
  r,
  busy,
  onSubmit,
  onDrop,
  onCancel,
  onFinalise,
}: {
  r: Registration;
  busy: boolean;
  onSubmit?: () => void;
  onDrop?: (lineId: string) => void;
  onCancel?: () => void;
  onFinalise?: () => void;
}) {
  const editable = r.status === "draft" || r.status === "returned" || r.status === "registered";
  const actions: RowAction[] = [];
  if (onSubmit && (r.status === "draft" || r.status === "returned")) actions.push({ id: "registration-submit", label: "Submit", variant: "primary", onClick: onSubmit });
  if (onFinalise && r.status === "registered") actions.push({ id: "registration-finalise", label: "Finalise", onClick: onFinalise });
  if (onCancel && !["finalised", "cancelled", "archived"].includes(r.status)) actions.push({ id: "registration-cancel", label: "Cancel registration", onClick: onCancel });
  return (
    <BentoTile span={12} title={`Term ${r.termNo} registration`} action={<StatusBadge status={r.status} />}>
      <div data-registration={r.id}>
        <p className="is-muted">
          {credits(r.totalCredits)} · {r.origin === "self" ? "registered by the student" : "registered by staff"}
          {r.statusReason ? ` · ${r.statusReason}` : ""}
        </p>
        <DataTable
          caption="Registered components"
          rows={r.lines}
          rowKey={(l) => l.id}
          empty={<EmptyState title="No components selected" />}
          columns={[
            { key: "slot", header: "Component", render: (l) => `${l.slotCode} · ${l.title}` },
            { key: "kind", header: "Kind", render: (l) => statusLabel(l.kind), muted: true },
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
        <Actions disabled={busy} actions={actions} />
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
  return (
    <div data-form="new-registration">
      <Select label="Window" value={term} onChange={(e) => setTerm(e.target.value)} options={open.map((w) => ({ value: w.academicTermId, label: `${w.name} · closes ${fmtDate(w.closesOn)}` }))} />
      <ul className="sb-list">
        {choices.map((c) => (
          <li key={c.curriculumComponentId}>
            <label>
              <input
                type="checkbox"
                data-component={c.slotCode}
                checked={!!picked[c.curriculumComponentId]}
                onChange={(e) => setPicked((p) => ({ ...p, [c.curriculumComponentId]: e.target.checked }))}
              />{" "}
              {c.slotCode} · {c.title} · {credits(c.credits)}
              {c.suggestedKind && c.suggestedKind !== "regular" ? ` · ${statusLabel(c.suggestedKind)}` : ""}
            </label>
          </li>
        ))}
      </ul>
      <p className="is-muted">{credits(total)} selected</p>
      <Button
        size="sm"
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
      </Button>
    </div>
  );
}

// ================================================================== Student 360: registration

export function Student360Registration({ studentId }: { studentId: string }) {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const regs = useQuery({ queryKey: rkeys.scoped(studentId, "registrations"), queryFn: () => api.listRegistrations(studentId) });
  const opts = useQuery({ queryKey: rkeys.scoped(studentId, "options"), queryFn: () => api.options(studentId) });
  const holds = useQuery({ queryKey: rkeys.scoped(studentId, "holds"), queryFn: () => api.listHolds(studentId) });
  const placeHold = () =>
    ask({
      title: "Place registration hold",
      name: "hold-place",
      fields: [{ name: "source", label: "Source", type: "select", options: HOLD_SOURCES.map((s) => ({ value: s, label: statusLabel(s) })), initial: "administrative" }, REASON],
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
          Finance: {statusLabel(holds.data?.financeEvidence.kind ?? "—")}
        </p>
        <DataTable
          caption="Holds"
          rows={holds.data?.items ?? []}
          rowKey={(h) => h.id}
          empty={<EmptyState title="No holds" />}
          columns={[
            { key: "src", header: "Source", render: (h) => statusLabel(h.source) },
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
      {(regs.data ?? []).length === 0 && (
        <BentoTile span={12}>
          <EmptyState icon={CalendarCheck} title="No registrations yet" />
        </BentoTile>
      )}
      {(regs.data ?? []).map((r) => (
        <RegistrationCard
          key={r.id}
          r={r}
          busy={busy}
          onSubmit={() => void run(() => api.registrationCommand(r.id, r.version, "submit"), (x) => `Registration ${statusLabel(x.status).toLowerCase()}`)}
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
  const { run, submit, busy, dialog } = useCommand();
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
        { name: "outcome", label: "Outcome", type: "select", options: [{ value: "", label: "Policy recommendation" }, ...OUTCOMES.map((o) => ({ value: o, label: statusLabel(o) }))] },
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
      onSubmit: (v) => submit(() => api.requestBreak(studentId, { reason: v.reason!, startsOn: v.startsOn!, expectedReturnOn: v.expectedReturnOn! }), (b) => `Break ${statusLabel(b.status).toLowerCase()}`),
    });
  return (
    <>
      <StatTile span={3} icon={GraduationCap} tone="violet" label="Standing" value={s ? statusLabel(s.standing) : "—"} />
      <StatTile span={3} icon={CalendarCheck} tone="blue" label="Current term" value={String(s?.currentTermNo ?? "—")} />
      <StatTile span={3} icon={Layers} tone="green" label="Credits" value={s ? String(s.totalCredits) : "—"} />
      <StatTile span={3} icon={Award} tone="amber" label="Qualification" value={elig ? (elig.eligible ? "Eligible" : "Pending") : "Not evaluated"} />
      <BentoTile
        span={12}
        title="Progression decisions"
        data-slice="3B"
        action={
          <Button size="sm" data-action="decision-new" onClick={recordDecision}>
            Record decision
          </Button>
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
                  {statusLabel(d.outcome)}
                  {d.recommendedOutcome && d.recommendedOutcome !== d.outcome ? ` (policy: ${statusLabel(d.recommendedOutcome)})` : ""}
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
                      { id: "decision-submit", label: "Make effective", variant: "primary", onClick: () => void run(() => api.decisionCommand(d.id, d.version, "submit"), (x) => `Decision ${statusLabel(x.status).toLowerCase()}`) },
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
            { key: "k", header: "Entry", render: (e) => statusLabel(e.kind) },
            { key: "c", header: "Credits", render: (e) => (e.kind === "revoked" || e.kind === "expired" ? `−${e.credits}` : `${e.credits}`) },
            { key: "src", header: "Source", render: (e) => statusLabel(e.sourceType), muted: true },
            { key: "at", header: "Recorded", render: (e) => fmtDate(e.recordedAt.slice(0, 10)), muted: true },
          ]}
        />
        {lastAbc && (
          <p className="is-muted">
            Last ABC sync: {statusLabel(lastAbc.status)}
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
              {elig.outcomes.length ? ` · ${elig.outcomes.map(statusLabel).join(", ")}` : ""}
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
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== student-facing

/** Student-facing `/registration` (self-registration, add/drop, cancellation). */
export function StudentRegistrationScreen() {
  const api = useRegistrationApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const opts = useQuery({ queryKey: rkeys.scoped("me", "options"), queryFn: () => api.myOptions() });
  const mine = useQuery({ queryKey: rkeys.scoped("me", "registrations"), queryFn: () => api.myRegistrations() });
  const live = (mine.data ?? []).filter((r) => r.status !== "cancelled");
  return (
    <>
      <BentoTile span={12} variant="accent" data-slice="3B">
        <span className="sb-chip">
          <CalendarCheck strokeWidth={1.75} /> Registration
        </span>
        <h1>My registration</h1>
        <p>{opts.data ? `Term ${opts.data.termNo ?? "—"} · ${statusLabel(opts.data.standing)}` : "Loading…"}</p>
      </BentoTile>
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
          onSubmit={() => void run(() => api.submitMine(r.id, r.version), (x) => `Registration ${statusLabel(x.status).toLowerCase()}`)}
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
