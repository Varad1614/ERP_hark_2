/**
 * Slice **4B** web components (timetable, sessions + attendance, monitoring, internal assessment
 * and coursework). The export names are imported by the route frames (docs/batch4-contracts.md
 * §10).
 *
 * - `TimetableAdminTab`, `AttendanceAdminTab`, `MonitoringAdminTab`, `AssessmentAdminTab` — Admin → Academic delivery tabs.
 * - `StudentTimetableScreen` / `FacultyTimetableScreen` — `/timetable` by role.
 * - `StudentAssignmentsScreen` — student `/assignments`.
 * - `FacultyAttendanceScreen` — faculty `/attendance`.
 */
import { BookOpenCheck, CalendarDays, ClipboardCheck, Plus, ShieldAlert } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, Modal, Select } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, NoInstitution, REASON, todayIso, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { DAY, dkeys, useDeliveryApi } from "./hooks";
import type { AttendanceMark, TeachingSession, TimetableCommand, TimetableSlot } from "./types";

export { DeliveryApiProvider } from "./hooks";

const MARKS: AttendanceMark[] = ["present", "absent", "late", "excused"];
const pct = (n: number) => `${Math.round(n * 10) / 10}%`;
/** No finalised sessions yet: there is no attendance to report, so show a dash rather than 100%. */
const attendancePct = (l: { total: number; percent: number }) => (l.total > 0 ? pct(l.percent) : "—");
const when = (iso: string | null | undefined) => (iso ? new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }).format(new Date(iso)) : "—");

/** Published terms of the institution's calendars. */
function useTerms(institutionId: string | null | undefined) {
  const inst = useInstitutionApi();
  const cals = useQuery({ queryKey: dkeys.scoped(institutionId ?? "-", "calendars"), queryFn: () => inst.listCalendars(institutionId!), enabled: !!institutionId });
  const versions = useQueries({ queries: (cals.data ?? []).map((c) => ({ queryKey: dkeys.scoped(c.id, "calendar-versions"), queryFn: () => inst.listVersions(c.id) })) });
  return versions
    .flatMap((q) => q.data ?? [])
    .filter((v) => v.status === "published")
    .flatMap((v) => v.terms.map((t) => ({ value: t.id, label: `${t.name} (${t.code})` })));
}

function TermPicker({ terms, value, onChange }: { terms: { value: string; label: string }[]; value: string; onChange: (v: string) => void }) {
  return <Select label="Academic term" value={value} onChange={(e) => onChange(e.target.value)} options={[{ value: "", label: "Choose a term" }, ...terms]} data-term-picker />;
}

function useSections(institutionId: string | undefined, termId: string) {
  const api = useDeliveryApi();
  return useQuery({ queryKey: dkeys.scoped(`${institutionId}:${termId}`, "sections"), queryFn: () => api.sections(institutionId!, termId), enabled: !!institutionId && !!termId });
}

function SectionPicker({ institutionId, term, value, onChange }: { institutionId: string; term: string; value: string; onChange: (v: string) => void }) {
  const sections = useSections(institutionId, term);
  return (
    <Select
      label="Section"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      options={[{ value: "", label: "Choose a section" }, ...(sections.data ?? []).map((s) => ({ value: s.sectionId, label: `${s.offeringCode} · ${s.sectionCode}` }))]}
      data-section-picker
    />
  );
}

/** Weekly grid: one column per weekday with its slots. */
function WeekGrid({ slots, empty }: { slots: TimetableSlot[]; empty: string }) {
  if (slots.length === 0)
    return (
      <EmptyState icon={CalendarDays} title="No classes scheduled">
        {empty}
      </EmptyState>
    );
  const days = [1, 2, 3, 4, 5, 6, 7].filter((d) => d <= 5 || slots.some((s) => s.dayOfWeek === d));
  return (
    <div className="sb-week" data-week-grid style={{ display: "grid", gridTemplateColumns: `repeat(${days.length}, minmax(0, 1fr))`, gap: "0.75rem" }}>
      {days.map((d) => (
        <section key={d} data-day={d}>
          <h3>{DAY[d]}</h3>
          {slots
            .filter((s) => s.dayOfWeek === d)
            .sort((a, b) => a.startsAt.localeCompare(b.startsAt))
            .map((s) => (
              <article key={s.id} className="sb-slot" data-slot={s.id} style={{ padding: "0.5rem", borderRadius: "0.75rem", border: "1px solid var(--sb-border, #ddd)", marginBottom: "0.5rem" }}>
                <b>
                  {s.startsAt}–{s.endsAt}
                </b>
                <div>
                  {s.offeringCode} · {s.sectionCode}
                </div>
                <small>{s.title}</small>
                <div>
                  <small>
                    {s.roomLabel ?? "Room to be announced"}
                    {s.facultyNames.length ? ` · ${s.facultyNames.join(", ")}` : ""}
                  </small>
                </div>
              </article>
            ))}
        </section>
      ))}
    </div>
  );
}

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="4B">
      <NoInstitution />
    </BentoTile>
  );
}

// ================================================================== admin: timetable

export function TimetableAdminTab({ institution }: { institution: Institution | null }) {
  const api = useDeliveryApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const terms = useTerms(institution?.id);
  const [term, setTerm] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const sections = useSections(institution?.id, term);
  const reqs = useQuery({ queryKey: dkeys.scoped(`${institution?.id}:${term}`, "requirements"), queryFn: () => api.listRequirements(institution!.id, term), enabled: !!institution && !!term });
  const tts = useQuery({ queryKey: dkeys.scoped(`${institution?.id}:${term}`, "timetables"), queryFn: () => api.listTimetables(institution!.id, term), enabled: !!institution && !!term });
  const detail = useQuery({ queryKey: dkeys.scoped(selected ?? "-", "timetable"), queryFn: () => api.getTimetable(selected!), enabled: !!selected });
  if (!institution) return <NoInstitutionTile />;
  return (
    <>
      <BentoTile span={12} title="Timetable" data-slice="4B">
        <TermPicker terms={terms} value={term} onChange={setTerm} />
      </BentoTile>
      {term && (
        <>
          <BentoTile span={6} title="Scheduling requirements">
            <DataTable
              caption="Scheduling requirements"
              rows={sections.data ?? []}
              rowKey={(s) => s.sectionId}
              empty={
                <EmptyState icon={CalendarDays} title="No sections in this term">
                  Offerings and sections come from Academic delivery → Offerings.
                </EmptyState>
              }
              columns={[
                { key: "s", header: "Section", render: (s) => <span data-section={s.sectionId}>{`${s.offeringCode} · ${s.sectionCode}`}</span> },
                {
                  key: "r",
                  header: "Requirement",
                  render: (s) => {
                    const r = reqs.data?.find((x) => x.sectionId === s.sectionId && x.status === "active");
                    return r ? `${r.sessionsPerWeek} × ${r.sessionMinutes} min${r.roomLabel ? ` · ${r.roomLabel}` : ""}` : "—";
                  },
                  muted: true,
                },
                {
                  key: "a",
                  header: "",
                  render: (s) =>
                    reqs.data?.some((x) => x.sectionId === s.sectionId && x.status === "active") ? null : (
                      <Actions
                        disabled={busy}
                        actions={[
                          {
                            id: "add-requirement",
                            label: "Set requirement",
                            onClick: () =>
                              ask({
                                title: `Scheduling requirement · ${s.offeringCode}`,
                                name: "requirement",
                                submitLabel: "Save",
                                fields: [
                                  { name: "sessionsPerWeek", label: "Sessions a week", type: "number", required: true, initial: "3" },
                                  { name: "sessionMinutes", label: "Minutes per session", type: "number", required: true, initial: "60" },
                                  { name: "roomLabel", label: "Room", placeholder: "R101" },
                                ],
                                onSubmit: (v) =>
                                  submit(
                                    () => api.createRequirement(institution.id, { sectionId: s.sectionId, sessionsPerWeek: Number(v.sessionsPerWeek), sessionMinutes: Number(v.sessionMinutes), roomLabel: v.roomLabel || null }),
                                    "Requirement saved",
                                  ),
                              }),
                          },
                        ]}
                      />
                    ),
                },
              ]}
            />
          </BentoTile>
          <BentoTile
            span={6}
            title="Timetable versions"
            action={
              <Button
                size="sm"
                icon={<Plus strokeWidth={1.75} />}
                data-new-timetable
                onClick={() =>
                  ask({
                    title: "New timetable",
                    name: "new-timetable",
                    submitLabel: "Start planning",
                    fields: [{ name: "name", label: "Name", required: true, initial: "Term timetable", wide: true }],
                    onSubmit: (v) => submit(() => api.createTimetable(institution.id, { academicTermId: term, name: v.name! }), "Timetable started"),
                  })
                }
              >
                New timetable
              </Button>
            }
          >
            <DataTable
              caption="Timetables"
              rows={tts.data ?? []}
              rowKey={(t) => t.id}
              empty={
                <EmptyState icon={CalendarDays} title="No timetable yet">
                  Start one, generate it from the requirements, validate and publish.
                </EmptyState>
              }
              columns={[
                {
                  key: "n",
                  header: "Version",
                  render: (t) => (
                    <button type="button" className="sb-link" data-timetable={t.id} onClick={() => setSelected(t.id)}>
                      {`${t.name} v${t.versionNo}`}
                    </button>
                  ),
                },
                { key: "s", header: "Status", render: (t) => <StatusBadge status={t.pendingDecision ? "pending_approval" : t.status} /> },
                {
                  key: "a",
                  header: "",
                  render: (t) => {
                    const go = (cmd: TimetableCommand, msg: string) => () => {
                      setSelected(t.id);
                      void run(() => api.timetableCommand(t.id, t.version, cmd), msg);
                    };
                    const a: RowAction[] = [];
                    if (t.status === "planning" || t.status === "draft") a.push({ id: "tt-generate", label: "Generate", onClick: go("generate", "Draft generated") });
                    if (t.status === "draft") a.push({ id: "tt-validate", label: "Validate", variant: "primary", onClick: go("validate", "Validated") });
                    if (t.status === "validated" && !t.pendingDecision) a.push({ id: "tt-submit", label: "Submit for approval", variant: "primary", onClick: go("submit", "Submitted") });
                    if (t.status === "approved") a.push({ id: "tt-publish", label: "Publish", variant: "primary", onClick: go("publish", "Published") });
                    if (t.status === "published") a.push({ id: "tt-activate", label: "Activate", variant: "primary", onClick: go("activate", "Timetable is operational") });
                    if (t.status === "active") {
                      a.push({
                        id: "tt-revise",
                        label: "Revise",
                        onClick: () => ask({ title: "Request a revision", name: "tt-revise", fields: [REASON], submitLabel: "Create revised draft", onSubmit: (v) => submit(() => api.requestRevision(t.id, v.reason!), "Revised draft created") }),
                      });
                      a.push({
                        id: "tt-retire",
                        label: "Retire",
                        onClick: () => ask({ title: "Retire timetable", name: "tt-retire", fields: [REASON], submitLabel: "Retire", onSubmit: (v) => submit(() => api.timetableCommand(t.id, t.version, "retire", v.reason), "Retired") }),
                      });
                    }
                    if (t.status === "retired" || t.status === "revised") a.push({ id: "tt-archive", label: "Archive", onClick: go("archive", "Archived") });
                    return <Actions disabled={busy} actions={a} />;
                  },
                },
              ]}
            />
          </BentoTile>
          {detail.data && (
            <BentoTile span={12} title={`${detail.data.name} v${detail.data.versionNo} · weekly slots`} data-timetable-detail>
              {detail.data.validationReport && detail.data.validationReport.conflicts.length > 0 && (
                <ul data-conflicts>
                  {detail.data.validationReport.conflicts.map((c, i) => (
                    <li key={i}>
                      <Badge kind={c.severity === "critical" ? "attention" : "review"}>{c.severity}</Badge> {c.message}
                    </li>
                  ))}
                </ul>
              )}
              <WeekGrid slots={detail.data.slots} empty="Generate the draft from the scheduling requirements." />
            </BentoTile>
          )}
        </>
      )}
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: attendance

export function AttendanceAdminTab({ institution }: { institution: Institution | null }) {
  const api = useDeliveryApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const terms = useTerms(institution?.id);
  const [term, setTerm] = useState("");
  const [section, setSection] = useState("");
  const policies = useQuery({ queryKey: dkeys.scoped(institution?.id ?? "-", "attendance-policies"), queryFn: () => api.listPolicies(institution!.id), enabled: !!institution });
  const summary = useQuery({ queryKey: dkeys.scoped(section || "-", "attendance-summary"), queryFn: () => api.summary(section), enabled: !!section });
  if (!institution) return <NoInstitutionTile />;
  return (
    <>
      <BentoTile
        span={12}
        title="Attendance policy"
        data-slice="4B"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-policy
            onClick={() =>
              ask({
                title: "New attendance policy",
                name: "attendance-policy",
                submitLabel: "Save draft",
                fields: [
                  { name: "name", label: "Name", required: true, initial: "Minimum attendance", wide: true },
                  { name: "thresholdPercent", label: "Threshold (%)", type: "number", required: true, initial: "75" },
                  { name: "minSessions", label: "Evaluate after (sessions)", type: "number", initial: "3" },
                ],
                onSubmit: (v) => submit(() => api.createPolicy(institution.id, { name: v.name!, thresholdPercent: Number(v.thresholdPercent), minSessions: v.minSessions ? Number(v.minSessions) : undefined }), "Policy drafted"),
              })
            }
          >
            New policy
          </Button>
        }
      >
        <DataTable
          caption="Attendance policies"
          rows={policies.data ?? []}
          rowKey={(p) => p.id}
          empty={
            <EmptyState icon={ClipboardCheck} title="No attendance policy">
              Without an effective policy the default 75 % rule is shown and no breach is raised.
            </EmptyState>
          }
          columns={[
            { key: "n", header: "Policy", render: (p) => <span data-policy={p.id}>{p.name}</span> },
            { key: "t", header: "Threshold", render: (p) => `${p.thresholdPercent}% after ${p.minSessions} session(s)`, muted: true },
            { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "a",
              header: "",
              render: (p) => <Actions disabled={busy} actions={p.status === "draft" ? [{ id: "policy-submit", label: "Submit for approval", variant: "primary", onClick: () => void run(() => api.submitPolicy(p.id, p.version), "Policy submitted") }] : []} />,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Section attendance">
        <TermPicker
          terms={terms}
          value={term}
          onChange={(v) => {
            setTerm(v);
            setSection("");
          }}
        />
        {term && <SectionPicker institutionId={institution.id} term={term} value={section} onChange={setSection} />}
        {summary.data && (
          <DataTable
            caption="Attendance summary"
            rows={summary.data.items}
            rowKey={(l) => l.studentId}
            empty={
              <EmptyState icon={ClipboardCheck} title="No students seated">
                Seat students in the section first.
              </EmptyState>
            }
            columns={[
              { key: "n", header: "Student", render: (l) => `${l.displayName} (${l.studentNumber})` },
              { key: "c", header: "Attended", render: (l) => `${l.attended} / ${l.total}`, muted: true },
              { key: "p", header: "Attendance", render: (l) => (l.belowThreshold ? <Badge kind="attention">{`${pct(l.percent)} · below ${summary.data.thresholdPercent}%`}</Badge> : attendancePct(l)) },
            ]}
          />
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: monitoring

export function MonitoringAdminTab({ institution }: { institution: Institution | null }) {
  const api = useDeliveryApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const terms = useTerms(institution?.id);
  const [term, setTerm] = useState("");
  const sections = useSections(institution?.id, term);
  const cases = useQuery({ queryKey: dkeys.scoped(institution?.id ?? "-", "monitoring-cases"), queryFn: () => api.listCases(institution!.id), enabled: !!institution });
  if (!institution) return <NoInstitutionTile />;
  return (
    <>
      <BentoTile
        span={12}
        title="Academic monitoring"
        data-slice="4B"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-case
            disabled={!term}
            onClick={() =>
              ask({
                title: "Open a monitoring case",
                name: "monitoring-case",
                submitLabel: "Open case",
                fields: [
                  {
                    name: "sectionId",
                    label: "Section",
                    type: "select",
                    required: true,
                    options: [{ value: "", label: "Choose a section" }, ...(sections.data ?? []).map((s) => ({ value: s.sectionId, label: `${s.offeringCode} · ${s.sectionCode}` }))],
                    wide: true,
                  },
                  { name: "title", label: "Title", required: true, initial: "Attendance watch", wide: true },
                  { name: "reviewerSubject", label: "Independent reviewer (login)", initial: "dev|meera.shah@sutra.dev", wide: true },
                ],
                onSubmit: (v) => submit(() => api.openCase(institution.id, { scopeKind: "section", sectionId: v.sectionId!, title: v.title!, reviewerSubject: v.reviewerSubject || null }), "Case opened"),
              })
            }
          >
            Open case
          </Button>
        }
      >
        <TermPicker terms={terms} value={term} onChange={setTerm} />
        <DataTable
          caption="Monitoring cases"
          rows={cases.data ?? []}
          rowKey={(c) => c.id}
          empty={
            <EmptyState icon={ShieldAlert} title="No monitoring cases">
              Choose a term and open one for a section to assess its academic risk.
            </EmptyState>
          }
          columns={[
            { key: "t", header: "Case", render: (c) => <span data-case={c.id}>{`${c.title}${c.cycleNo > 1 ? ` (cycle ${c.cycleNo})` : ""}`}</span> },
            {
              key: "r",
              header: "Risk",
              render: (c) => (c.riskAssessments[0] ? <Badge kind={["high", "critical"].includes(c.riskAssessments[0].level) ? "attention" : "info"}>{statusLabel(c.riskAssessments[0].level)}</Badge> : "—"),
            },
            { key: "s", header: "Status", render: (c) => <StatusBadge status={c.pendingDecision ? "pending_approval" : c.status} /> },
            {
              key: "a",
              header: "",
              render: (c) => {
                const a: RowAction[] = [];
                if (["open", "reopened", "under_assessment", "intervention"].includes(c.status))
                  a.push({ id: "case-assess", label: "Assess risk", onClick: () => void run(() => api.assessRisk(c.id, "attendance", "moderate", "Reviewer judgement"), "Risk assessed") });
                if (c.riskAssessments.length && !c.outcomePublishedAt && c.status !== "closed")
                  a.push({
                    id: "case-outcome",
                    label: "Publish outcome",
                    onClick: () =>
                      ask({ title: "Publish monitoring outcome", name: "case-outcome", fields: [{ name: "summary", label: "Summary", required: true, wide: true }], submitLabel: "Publish", onSubmit: (v) => submit(() => api.publishOutcome(c.id, c.version, v.summary!), "Outcome published") }),
                  });
                if (["under_assessment", "intervention"].includes(c.status) && !c.pendingDecision)
                  a.push({
                    id: "case-review",
                    label: "Governance review",
                    onClick: () =>
                      ask({
                        title: "Submit to governance review",
                        name: "case-review",
                        submitLabel: "Submit",
                        fields: [{ name: "interventionTitle", label: "Corrective intervention (leave empty for none)", wide: true }],
                        onSubmit: (v) => submit(() => api.submitReview(c.id, c.version, { requiresIntervention: !!v.interventionTitle, interventionTitle: v.interventionTitle || undefined }), "Review decided"),
                      }),
                  });
                for (const iv of c.interventions) {
                  if (iv.status === "planned" && !iv.ownerSubject)
                    a.push({
                      id: "iv-assign",
                      label: `Assign: ${iv.title}`,
                      onClick: () =>
                        ask({
                          title: "Assign intervention owner",
                          name: "iv-assign",
                          fields: [{ name: "ownerSubject", label: "Owner (login)", required: true, initial: "dev|meera.shah@sutra.dev", wide: true }],
                          submitLabel: "Assign",
                          onSubmit: (v) => submit(() => api.interventionCommand(c.id, iv.id, iv.version, "assign", { ownerSubject: v.ownerSubject! }), "Owner assigned"),
                        }),
                    });
                  if (iv.status === "planned" && iv.ownerSubject) a.push({ id: "iv-trigger", label: `Trigger: ${iv.title}`, onClick: () => void run(() => api.interventionCommand(c.id, iv.id, iv.version, "trigger"), "Intervention triggered") });
                  if (iv.status === "triggered") a.push({ id: "iv-start", label: "Start intervention", onClick: () => void run(() => api.interventionCommand(c.id, iv.id, iv.version, "start"), "Started") });
                  if (iv.status === "in_progress")
                    a.push({
                      id: "iv-complete",
                      label: "Complete intervention",
                      onClick: () =>
                        ask({
                          title: "Intervention outcome",
                          name: "iv-complete",
                          fields: [{ name: "outcome", label: "Outcome", required: true, wide: true }],
                          submitLabel: "Complete",
                          onSubmit: (v) => submit(() => api.interventionCommand(c.id, iv.id, iv.version, "complete", { outcome: v.outcome! }), "Completed"),
                        }),
                    });
                }
                if (["under_assessment", "intervention"].includes(c.status) && c.outcomePublishedAt && !c.pendingDecision)
                  a.push({ id: "case-certify", label: "Certify", variant: "primary", onClick: () => void run(() => api.certify(c.id, c.version), "Certified") });
                if (c.status === "certified") a.push({ id: "case-close", label: "Close", variant: "primary", onClick: () => void run(() => api.closeCase(c.id, c.version, "Monitoring complete"), "Case closed") });
                return <Actions disabled={busy} actions={a} />;
              },
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: internal assessment

export function AssessmentAdminTab({ institution }: { institution: Institution | null }) {
  const api = useDeliveryApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const terms = useTerms(institution?.id);
  const [term, setTerm] = useState("");
  const [section, setSection] = useState("");
  const bps = useQuery({ queryKey: dkeys.scoped(section || "-", "blueprints"), queryFn: () => api.listBlueprints(section), enabled: !!section });
  const wins = useQuery({ queryKey: dkeys.scoped(section || "-", "windows"), queryFn: () => api.listWindows(section), enabled: !!section });
  const results = useQuery({ queryKey: dkeys.scoped(section || "-", "results"), queryFn: () => api.listResults(section), enabled: !!section });
  if (!institution) return <NoInstitutionTile />;
  const published = bps.data?.find((b) => b.status === "published");
  return (
    <>
      <BentoTile span={12} title="Internal assessment" data-slice="4B">
        <TermPicker
          terms={terms}
          value={term}
          onChange={(v) => {
            setTerm(v);
            setSection("");
          }}
        />
        {term && <SectionPicker institutionId={institution.id} term={term} value={section} onChange={setSection} />}
      </BentoTile>
      {section && (
        <>
          <BentoTile
            span={6}
            title="Blueprints"
            action={
              <Button
                size="sm"
                icon={<Plus strokeWidth={1.75} />}
                data-new-blueprint
                onClick={() =>
                  ask({
                    title: "New blueprint",
                    name: "blueprint",
                    submitLabel: "Save draft",
                    fields: [
                      { name: "title", label: "Title", required: true, initial: "Internal assessment", wide: true },
                      { name: "totalMarks", label: "Total marks", type: "number", required: true, initial: "50" },
                      { name: "assignmentWeight", label: "Assignment weight (%)", type: "number", required: true, initial: "40" },
                    ],
                    onSubmit: (v) => {
                      const w = Number(v.assignmentWeight);
                      return submit(
                        () =>
                          api.createBlueprint(section, {
                            title: v.title!,
                            totalMarks: Number(v.totalMarks),
                            components: [
                              { key: "a1", title: "Assignments", kind: "assignment", weight: w },
                              { key: "t1", title: "Tests", kind: "test", weight: 100 - w },
                            ],
                          }),
                        "Blueprint drafted",
                      );
                    },
                  })
                }
              >
                New blueprint
              </Button>
            }
          >
            <DataTable
              caption="Blueprints"
              rows={bps.data ?? []}
              rowKey={(b) => b.id}
              empty={
                <EmptyState icon={BookOpenCheck} title="No blueprint">
                  Draft the weighted components, then publish.
                </EmptyState>
              }
              columns={[
                { key: "t", header: "Blueprint", render: (b) => <span data-blueprint={b.id}>{b.title}</span> },
                { key: "c", header: "Components", render: (b) => b.components.map((c) => `${c.title} ${c.weight}%`).join(" · "), muted: true },
                { key: "s", header: "Status", render: (b) => <StatusBadge status={b.status} /> },
                {
                  key: "a",
                  header: "",
                  render: (b) => <Actions disabled={busy} actions={b.status === "draft" ? [{ id: "bp-publish", label: "Publish", variant: "primary", onClick: () => void run(() => api.publishBlueprint(b.id, b.version), "Blueprint published") }] : []} />,
                },
              ]}
            />
          </BentoTile>
          <BentoTile
            span={6}
            title="Assessment windows"
            action={
              published && (
                <Button
                  size="sm"
                  icon={<Plus strokeWidth={1.75} />}
                  data-new-window
                  onClick={() =>
                    ask({
                      title: "New assessment window",
                      name: "assessment-window",
                      submitLabel: "Create",
                      fields: [
                        { name: "componentKey", label: "Component", type: "select", required: true, options: published.components.map((c) => ({ value: c.key, label: c.title })), initial: published.components[0]?.key },
                        { name: "title", label: "Title", required: true, wide: true },
                        { name: "dueOn", label: "Due on", type: "date", required: true, initial: todayIso() },
                        { name: "maxMarks", label: "Maximum marks", type: "number", required: true, initial: "10" },
                      ],
                      onSubmit: (v) =>
                        submit(
                          () =>
                            api.createWindow(section, {
                              blueprintId: published.id,
                              componentKey: v.componentKey!,
                              title: v.title!,
                              opensAt: new Date().toISOString(),
                              dueAt: new Date(`${v.dueOn}T23:59:00`).toISOString(),
                              maxMarks: Number(v.maxMarks),
                            }),
                          "Window created",
                        ),
                    })
                  }
                >
                  New window
                </Button>
              )
            }
          >
            <DataTable
              caption="Assessment windows"
              rows={wins.data ?? []}
              rowKey={(w) => w.id}
              empty={
                <EmptyState icon={BookOpenCheck} title="No windows">
                  Create coursework or a test for a blueprint component.
                </EmptyState>
              }
              columns={[
                { key: "t", header: "Window", render: (w) => <span data-window={w.id}>{w.title}</span> },
                { key: "d", header: "Due", render: (w) => when(w.extendedDueAt ?? w.dueAt), muted: true },
                { key: "s", header: "Status", render: (w) => <StatusBadge status={w.status} /> },
                {
                  key: "a",
                  header: "",
                  render: (w) => {
                    const a: RowAction[] = [];
                    if (w.status === "scheduled") a.push({ id: "window-open", label: "Open", variant: "primary", onClick: () => void run(() => api.windowCommand(w.id, w.version, "open"), "Window opened") });
                    if (w.status === "open" || w.status === "extended") a.push({ id: "window-close", label: "Close", onClick: () => void run(() => api.windowCommand(w.id, w.version, "close"), "Window closed") });
                    return <Actions disabled={busy} actions={a} />;
                  },
                },
              ]}
            />
          </BentoTile>
          <BentoTile
            span={12}
            title="Internal assessment results"
            action={
              <Button size="sm" variant="primary" data-finalize onClick={() => void run(() => api.finalizeResults(section), (r) => `${r.length} result(s) finalised`)} disabled={busy}>
                Finalise
              </Button>
            }
          >
            <DataTable
              caption="Results"
              rows={results.data ?? []}
              rowKey={(r) => r.id}
              empty={
                <EmptyState icon={BookOpenCheck} title="Not finalised">
                  Close every window and mark every student, then finalise.
                </EmptyState>
              }
              columns={[
                { key: "n", header: "Student", render: (r) => `${r.displayName} (${r.studentNumber})` },
                { key: "m", header: "Marks", render: (r) => `${r.marksObtained} / ${r.maxMarks}` },
                { key: "v", header: "Version", render: (r) => `v${r.resultVersion}`, muted: true },
                { key: "s", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
              ]}
            />
          </BentoTile>
        </>
      )}
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== student / faculty timetable

export function StudentTimetableScreen() {
  const api = useDeliveryApi();
  const week = useQuery({ queryKey: dkeys.scoped("me", "timetable"), queryFn: () => api.myTimetable() });
  const att = useQuery({ queryKey: dkeys.scoped("me", "attendance"), queryFn: () => api.myAttendance() });
  return (
    <>
      <BentoTile span={12} title={week.data ? `My timetable · week of ${fmtDate(week.data.weekOf)}` : "My timetable"} data-slice="4B" data-student-timetable>
        <WeekGrid slots={week.data?.items ?? []} empty="Your classes appear here once your courses have an operational timetable." />
      </BentoTile>
      <BentoTile span={12} title="My attendance">
        <DataTable
          caption="My attendance"
          rows={att.data ?? []}
          rowKey={(l) => l.sectionId}
          empty={
            <EmptyState icon={ClipboardCheck} title="No attendance yet">
              Attendance appears once sessions are finalised.
            </EmptyState>
          }
          columns={[
            { key: "c", header: "Course", render: (l) => `${l.offeringCode} · ${l.title}` },
            { key: "n", header: "Sessions", render: (l) => `${l.attended} / ${l.total}`, muted: true },
            { key: "p", header: "Attendance", render: (l) => (l.belowThreshold ? <Badge kind="attention">{`${pct(l.percent)} · needs ${l.thresholdPercent}%`}</Badge> : <span data-attendance-ok>{attendancePct(l)}</span>) },
          ]}
        />
      </BentoTile>
    </>
  );
}

export function FacultyTimetableScreen() {
  const api = useDeliveryApi();
  const week = useQuery({ queryKey: dkeys.scoped("my", "timetable"), queryFn: () => api.teachingTimetable() });
  return (
    <BentoTile span={12} title={week.data ? `My teaching timetable · week of ${fmtDate(week.data.weekOf)}` : "My teaching timetable"} data-slice="4B" data-faculty-timetable>
      <WeekGrid slots={week.data?.items ?? []} empty="Sections you teach appear here once the timetable is operational." />
    </BentoTile>
  );
}

// ================================================================== student assignments

export function StudentAssignmentsScreen() {
  const api = useDeliveryApi();
  const { submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const q = useQuery({ queryKey: dkeys.scoped("me", "assignments"), queryFn: () => api.myAssignments() });
  return (
    <>
      <BentoTile span={12} title="Assignments" data-slice="4B" data-assignments>
        <DataTable
          caption="Assignments"
          rows={q.data ?? []}
          rowKey={(a) => a.id}
          empty={
            <EmptyState icon={BookOpenCheck} title="Nothing assigned">
              Open coursework of your courses appears here.
            </EmptyState>
          }
          columns={[
            { key: "t", header: "Assignment", render: (a) => <span data-assignment={a.id}>{`${a.title} · ${a.offeringCode}`}</span> },
            { key: "d", header: "Due", render: (a) => when(a.extendedDueAt ?? a.dueAt), muted: true },
            { key: "m", header: "Marks", render: (a) => (a.submission?.marks !== null && a.submission?.marks !== undefined ? `${a.submission.marks} / ${a.maxMarks}` : `— / ${a.maxMarks}`), muted: true },
            { key: "s", header: "Status", render: (a) => <StatusBadge status={a.submission?.status ?? (a.status === "closed" ? "closed" : "pending")} /> },
            {
              key: "a",
              header: "",
              render: (a) =>
                !a.submission && (a.status === "open" || a.status === "extended") ? (
                  <Actions
                    actions={[
                      {
                        id: "assignment-submit",
                        label: "Submit",
                        variant: "primary",
                        onClick: () =>
                          ask({
                            title: `Submit · ${a.title}`,
                            name: "assignment-submit",
                            submitLabel: "Submit",
                            fields: [{ name: "content", label: "Your answer or a link to your work", required: true, wide: true }],
                            onSubmit: (v) => submit(() => api.submitAssignment(a.id, v.content!), "Submitted"),
                          }),
                      },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== faculty attendance

function SessionRoster({ session, onClose }: { session: TeachingSession; onClose: () => void }) {
  const api = useDeliveryApi();
  const { run, busy, dialog } = useCommand();
  const live = useQuery({ queryKey: dkeys.scoped(session.id, "session"), queryFn: () => api.getSession(session.id), initialData: session });
  const s = live.data ?? session;
  const [marks, setMarks] = useState<Record<string, AttendanceMark>>(() => Object.fromEntries(session.records.filter((r) => r.mark).map((r) => [r.studentId, r.mark as AttendanceMark])));
  const open = s.attendanceStatus !== "finalised";
  const canMark = open && ["started", "in_progress", "paused", "completed"].includes(s.status);
  return (
    <Modal open onClose={onClose} label="Session attendance">
      <h2>
        {fmtDate(s.sessionDate)} · {s.startsAt}–{s.endsAt}
      </h2>
      <p data-session-state>
        <StatusBadge status={s.status} /> <StatusBadge status={s.attendanceStatus} />
      </p>
      <div data-roster>
        {s.records.map((r) => (
          <div key={r.studentId} data-roster-student={r.studentNumber} style={{ display: "flex", gap: "0.5rem", alignItems: "center", marginBottom: "0.35rem", flexWrap: "wrap" }}>
            <span style={{ flex: 1 }}>
              {r.displayName} <small>({r.studentNumber})</small>
            </span>
            {MARKS.map((m) => (
              <Button key={m} size="sm" variant={(marks[r.studentId] ?? r.mark) === m ? "primary" : "ghost"} disabled={!canMark} data-mark={m} onClick={() => setMarks((x) => ({ ...x, [r.studentId]: m }))}>
                {m}
              </Button>
            ))}
          </div>
        ))}
      </div>
      <div className="sb-row-actions">
        {s.status === "scheduled" && (
          <Button variant="primary" disabled={busy} data-session-start onClick={() => void run(() => api.sessionCommand(s.id, s.version, "start"), "Session started")}>
            Start session
          </Button>
        )}
        {canMark && (
          <Button
            disabled={busy || Object.keys(marks).length === 0}
            data-save-marks
            onClick={() =>
              void run(
                () =>
                  api.capture(
                    s.id,
                    s.version,
                    Object.entries(marks).map(([studentId, mark]) => ({ studentId, mark })),
                  ),
                "Attendance saved",
              )
            }
          >
            Save marks
          </Button>
        )}
        {(s.status === "started" || s.status === "in_progress") && (
          <Button disabled={busy} data-session-complete onClick={() => void run(() => api.sessionCommand(s.id, s.version, "complete"), "Session completed")}>
            Complete session
          </Button>
        )}
        {open && s.attendanceStatus !== "not_started" && s.status !== "paused" && (
          <Button variant="primary" disabled={busy} data-finalise onClick={() => void run(() => api.finalise(s.id, s.version), "Attendance finalised")}>
            Finalise attendance
          </Button>
        )}
      </div>
      {dialog}
    </Modal>
  );
}

export function FacultyAttendanceScreen() {
  const api = useDeliveryApi();
  const { submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [open, setOpen] = useState<TeachingSession | null>(null);
  const q = useQuery({ queryKey: dkeys.scoped("my", "attendance"), queryFn: () => api.facultyAttendance() });
  const sections = q.data ?? [];
  return (
    <>
      <BentoTile span={12} variant="accent" data-slice="4B" data-faculty-attendance>
        <h1>Attendance</h1>
        {sections.length === 0 ? (
          <EmptyState icon={ClipboardCheck} title="No sections to teach">
            Sections appear here once you hold an active teaching assignment.
          </EmptyState>
        ) : (
          <p>Schedule a session, start it, mark the seated students and finalise the register.</p>
        )}
      </BentoTile>
      {sections.map((sec) => (
        <BentoTile
          key={sec.sectionId}
          span={12}
          title={`${sec.offeringCode} · ${sec.sectionCode} — ${sec.title} (${sec.seatedCount} seated)`}
          data-taught-section={sec.sectionId}
          action={
            <Button
              size="sm"
              icon={<Plus strokeWidth={1.75} />}
              data-new-session
              onClick={() =>
                ask({
                  title: `New session · ${sec.offeringCode}`,
                  name: "new-session",
                  submitLabel: "Schedule",
                  fields: [
                    { name: "sessionDate", label: "Date", type: "date", required: true, initial: todayIso() },
                    { name: "startsAt", label: "Starts (HH:MM)", required: true, initial: "09:00" },
                    { name: "endsAt", label: "Ends (HH:MM)", required: true, initial: "10:00" },
                    { name: "topic", label: "Topic", wide: true },
                  ],
                  onSubmit: (v) => submit(() => api.createSession(sec.sectionId, { sessionDate: v.sessionDate!, startsAt: v.startsAt, endsAt: v.endsAt, topic: v.topic || null }), "Session scheduled"),
                })
              }
            >
              New session
            </Button>
          }
        >
          <DataTable
            caption="Sessions"
            rows={sec.sessions}
            rowKey={(s) => s.id}
            empty={
              <EmptyState icon={CalendarDays} title="No sessions yet">
                Schedule today’s session to take attendance.
              </EmptyState>
            }
            columns={[
              {
                key: "d",
                header: "Session",
                render: (s) => (
                  <button type="button" className="sb-link" data-session={s.id} onClick={() => setOpen(s)}>
                    {`${fmtDate(s.sessionDate)} · ${s.startsAt}`}
                  </button>
                ),
              },
              { key: "t", header: "Topic", render: (s) => s.topic ?? "—", muted: true },
              { key: "s", header: "Session", render: (s) => <StatusBadge status={s.status} /> },
              { key: "a", header: "Attendance", render: (s) => <StatusBadge status={s.attendanceStatus} /> },
            ]}
          />
        </BentoTile>
      ))}
      {open && <SessionRoster session={open} onClose={() => setOpen(null)} />}
      {prompt}
      {dialog}
    </>
  );
}
