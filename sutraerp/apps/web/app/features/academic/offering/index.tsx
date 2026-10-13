/**
 * Slice **4A** web components (course offerings, sections, enrolment, faculty assignment). The
 * export names are imported by the route frames (docs/batch4-contracts.md §10).
 *
 * - `OfferingsAdminTab` — Admin → Academic delivery → Offerings: lifecycle commands (submit →
 *   approval through the WorkflowPort, publish, enrolment open / close, activate, suspend,
 *   freeze, governed unfreeze / cancellation, complete, archive), sections, capacity, roster,
 *   waitlist promotion.
 * - `FacultyAssignmentsAdminTab` — Admin → Academic delivery → Faculty assignment: who teaches
 *   each section (establish → approve → activate, revoke) and the governance policy versions.
 * - `StudentCoursesScreen` — student `/courses`: my seats + offerings open for enrolment.
 * - `FacultyClassesScreen` — faculty `/classes` (My classes).
 * - `FacultyStudentsScreen` — faculty `/my-students` (rosters of taught sections → read-only Student 360).
 */
import { BookOpen, GraduationCap, Layers, Plus, Presentation, ShieldCheck, Users } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal, StatTile } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router";
import { Actions, fmtDate, NoInstitution, QuickForm, REASON, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, useCommand, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { okeys, useOfferingApi } from "./hooks";
import type { FacultyRole, Offering, OfferingCommand, OfferingSection } from "./types";

export { OfferingApiProvider } from "./hooks";

const ROLES: FacultyRole[] = ["primary_instructor", "co_instructor", "tutor", "lab_instructor", "teaching_assistant", "course_coordinator"];
const roleLabel = (r: string) => r.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
const seats = (taken: number, cap: number | null | undefined) => (cap === null || cap === undefined ? `${taken} / ∞` : `${taken} / ${cap}`);
const facultyNames = (s: OfferingSection) =>
  s.faculty
    .filter((f) => f.status === "active")
    .map((f) => f.facultyName ?? f.facultySubjectRef)
    .join(", ") || "—";

/** Published terms of the institution's calendars. */
function useTerms(institutionId: string | null | undefined) {
  const inst = useInstitutionApi();
  const cals = useQuery({ queryKey: okeys.scoped(institutionId ?? "-", "calendars"), queryFn: () => inst.listCalendars(institutionId!), enabled: !!institutionId });
  const versions = useQueries({ queries: (cals.data ?? []).map((c) => ({ queryKey: okeys.scoped(c.id, "calendar-versions"), queryFn: () => inst.listVersions(c.id) })) });
  return versions
    .flatMap((q) => q.data ?? [])
    .filter((v) => v.status === "published")
    .flatMap((v) => v.terms.map((t) => ({ value: t.id, label: `${t.name} (${t.code}) · ${fmtDate(t.startsOn)} – ${fmtDate(t.endsOn)}` })));
}

interface ComponentLite {
  slotCode: string;
  title: string;
  termNo: number;
  credits: number;
}

/** Published curriculum versions of the institution's programmes with their components. */
function useCurriculumVersions(institutionId: string | null | undefined) {
  const inst = useInstitutionApi();
  const progs = useQuery({ queryKey: okeys.scoped(institutionId ?? "-", "programmes"), queryFn: () => inst.listProgrammes(institutionId!), enabled: !!institutionId });
  const curricula = useQueries({ queries: (progs.data ?? []).map((p) => ({ queryKey: okeys.scoped(p.id, "curricula"), queryFn: () => inst.listCurricula(p.id) })) });
  const versions = useQueries({
    queries: curricula.flatMap((q) => q.data ?? []).map((c) => ({ queryKey: okeys.scoped(c.id, "curriculum-versions"), queryFn: () => inst.listCurriculumVersions(c.id) })),
  });
  return versions
    .flatMap((q) => q.data ?? [])
    .filter((v) => v.status === "published")
    .map((v) => {
      const raw = v as unknown as { id: string; versionNo: number; components?: ComponentLite[]; content?: { components?: ComponentLite[] } };
      return { id: raw.id, label: `Curriculum v${raw.versionNo}`, components: raw.components ?? raw.content?.components ?? [] };
    });
}

// ================================================================== admin: offerings

const NEXT_COMMANDS: Record<string, { cmd: OfferingCommand; label: string; reason?: boolean; primary?: boolean }[]> = {
  draft: [{ cmd: "submit", label: "Submit for approval", primary: true }],
  approved: [{ cmd: "publish", label: "Publish", primary: true }],
  published: [{ cmd: "open-enrolment", label: "Open enrolment", primary: true }],
  enrollment_open: [{ cmd: "close-enrolment", label: "Close enrolment" }],
  enrollment_closed: [
    { cmd: "activate", label: "Activate", primary: true },
    { cmd: "open-enrolment", label: "Reopen enrolment" },
  ],
  active: [
    { cmd: "suspend", label: "Suspend", reason: true },
    { cmd: "freeze", label: "Freeze", reason: true },
    { cmd: "complete", label: "Complete" },
  ],
  suspended: [{ cmd: "reactivate", label: "Reactivate", reason: true, primary: true }],
  completed: [{ cmd: "archive", label: "Archive" }],
  cancelled: [{ cmd: "archive", label: "Archive" }],
};

export function OfferingsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useOfferingApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [adding, setAdding] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const terms = useTerms(institution?.id);
  const cvs = useCurriculumVersions(institution?.id);
  const q = useQuery({ queryKey: okeys.scoped(institution?.id ?? "-", "offerings"), queryFn: () => api.listOfferings(institution!.id), enabled: !!institution });
  if (!institution)
    return (
      <BentoTile span={12} data-slice="4A">
        <NoInstitution />
      </BentoTile>
    );
  const rows = q.data ?? [];
  const current = rows.find((o) => o.id === selected) ?? null;
  const components = cvs.flatMap((v) => v.components.map((c) => ({ value: `${v.id}|${c.slotCode}`, label: `${c.slotCode} · ${c.title} (${c.credits} cr, term ${c.termNo}) — ${v.label}` })));

  const actionsOf = (o: Offering): RowAction[] => {
    const acts: RowAction[] = (NEXT_COMMANDS[o.status] ?? []).map((c) => ({
      id: `offering-${c.cmd}`,
      label: c.label,
      variant: c.primary ? "primary" : "ghost",
      onClick: () =>
        c.reason
          ? ask({ title: `${c.label}: ${o.offeringCode}`, name: `offering-${c.cmd}`, fields: [REASON], submitLabel: c.label, onSubmit: (v) => submit(() => api.offeringCommand(o.id, o.version, c.cmd, v.reason!.trim()), `${o.offeringCode}: ${c.label.toLowerCase()} done`) })
          : void run(() => api.offeringCommand(o.id, o.version, c.cmd), (r) => `${o.offeringCode}: ${r.status.replace(/_/g, " ")}`),
    }));
    if (o.status === "pending_approval") {
      acts.push({ id: "offering-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideOffering(o.id, o.version, "approve"), `${o.offeringCode}: approved`) });
      acts.push({
        id: "offering-reject",
        label: "Return",
        onClick: () => ask({ title: `Return ${o.offeringCode}`, name: "offering-reject", fields: [REASON], submitLabel: "Return to draft", onSubmit: (v) => submit(() => api.decideOffering(o.id, o.version, "reject", v.reason!.trim()), "Returned to draft") }),
      });
    }
    if (o.status === "frozen")
      acts.push({
        id: "offering-unfreeze",
        label: "Unfreeze",
        variant: "primary",
        onClick: () => ask({ title: `Unfreeze ${o.offeringCode}`, name: "offering-unfreeze", fields: [REASON], submitLabel: "Request unfreeze", onSubmit: (v) => submit(() => api.governed(o.id, o.version, "unfreeze", v.reason!.trim()), (r) => `Unfreeze ${r.request.status}`) }),
      });
    if (["draft", "approved", "published", "suspended"].includes(o.status))
      acts.push({
        id: "offering-cancel",
        label: "Cancel",
        onClick: () => ask({ title: `Cancel ${o.offeringCode}`, name: "offering-cancel", fields: [REASON], submitLabel: "Request cancellation", onSubmit: (v) => submit(() => api.governed(o.id, o.version, "cancel", v.reason!.trim()), (r) => `Cancellation ${r.request.status}`) }),
      });
    acts.push({ id: "offering-manage", label: selected === o.id ? "Hide" : "Sections", onClick: () => setSelected(selected === o.id ? null : o.id) });
    return acts;
  };

  return (
    <>
      <BentoTile
        span={12}
        title="Course offerings"
        data-slice="4A"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-offering>
            New offering
          </Button>
        }
      >
        <DataTable
          caption="Course offerings"
          rows={rows}
          rowKey={(o) => o.id}
          empty={
            <EmptyState icon={BookOpen} title="No course offerings">
              Offer a curriculum component in a published term; it starts as a draft.
            </EmptyState>
          }
          columns={[
            { key: "code", header: "Offering", render: (o) => <span data-offering={o.offeringCode}>{o.offeringCode}</span> },
            { key: "title", header: "Course", render: (o) => `${o.title} · ${o.credits} cr`, muted: true },
            {
              key: "seats",
              header: "Seats",
              render: (o) => seats(o.seatsTaken, o.capacity ?? (o.sections.length > 0 && o.sections.every((s) => s.capacity !== null) ? o.sections.reduce((n, s) => n + (s.capacity ?? 0), 0) : null)),
              muted: true,
            },
            { key: "sections", header: "Sections", render: (o) => o.sections.filter((s) => s.status !== "cancelled" && s.status !== "merged").length, muted: true },
            {
              key: "status",
              header: "Status",
              render: (o) => (
                <span data-offering-status={o.status}>
                  <StatusBadge status={o.status} />
                </span>
              ),
            },
            { key: "a", header: "", render: (o) => <Actions disabled={busy} actions={actionsOf(o)} /> },
          ]}
        />
      </BentoTile>
      {current && <OfferingDetail offering={current} />}
      <Modal open={adding} onClose={() => setAdding(false)} label="New course offering">
        <h2>New course offering</h2>
        <QuickForm
          name="offering-create"
          submitLabel="Create draft"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "academicTermId", label: "Academic term", type: "select", required: true, options: [{ value: "", label: "Choose a published term" }, ...terms], wide: true },
            { name: "component", label: "Course component", type: "select", required: true, options: [{ value: "", label: "Choose a component" }, ...components], wide: true },
            { name: "offeringCode", label: "Offering code", required: true, placeholder: "CS101-T1" },
            { name: "title", label: "Title (optional)" },
            { name: "sectionCode", label: "First section", initial: "A" },
            { name: "sectionCapacity", label: "Section capacity", type: "number", initial: "60" },
          ]}
          onSubmit={async (v) => {
            const [curriculumVersionId, slotCode] = (v.component ?? "").split("|");
            const capacity = v.sectionCapacity ? Number(v.sectionCapacity) : null;
            await submit(
              () =>
                api.createOffering(institution.id, {
                  academicTermId: v.academicTermId!,
                  curriculumVersionId: curriculumVersionId!,
                  slotCode: slotCode!,
                  offeringCode: v.offeringCode!.trim(),
                  title: v.title?.trim() || null,
                  sections: v.sectionCode?.trim() ? [{ sectionCode: v.sectionCode.trim(), capacity }] : [],
                }),
              (o) => `${o.offeringCode}: draft created`,
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

function OfferingDetail({ offering: o }: { offering: Offering }) {
  const api = useOfferingApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [roster, setRoster] = useState<string | null>(null);
  const r = useQuery({ queryKey: okeys.scoped(roster ?? "-", "roster"), queryFn: () => api.roster(roster!), enabled: !!roster });
  const live = o.sections.filter((s) => s.status !== "merged");
  return (
    <>
      <BentoTile
        span={12}
        title={`${o.offeringCode} · sections`}
        data-offering-detail={o.offeringCode}
        action={
          <div className="sb-row-actions">
            <Button
              size="sm"
              variant="ghost"
              data-action="section-add"
              onClick={() =>
                ask({
                  title: `Add a section to ${o.offeringCode}`,
                  name: "section-add",
                  fields: [
                    { name: "sectionCode", label: "Section code", required: true, placeholder: "B" },
                    { name: "capacity", label: "Capacity", type: "number" },
                  ],
                  submitLabel: "Add section",
                  onSubmit: (v) => submit(() => api.addSection(o.id, { sectionCode: v.sectionCode!.trim(), capacity: v.capacity ? Number(v.capacity) : null }), "Section added"),
                })
              }
            >
              Add section
            </Button>
            {o.waitlisted > 0 && (
              <Button size="sm" data-action="waitlist-promote" onClick={() => void run(() => api.promote(o.id), (w) => `Seat offered to ${w.displayName}`)} disabled={busy}>
                Offer a seat ({o.waitlisted} waiting)
              </Button>
            )}
          </div>
        }
      >
        <DataTable
          caption="Sections"
          rows={live}
          rowKey={(s) => s.id}
          empty={
            <EmptyState icon={Layers} title="No sections">
              Add at least one section before submitting.
            </EmptyState>
          }
          columns={[
            { key: "code", header: "Section", render: (s) => <span data-section={s.sectionCode}>{s.sectionCode}</span> },
            { key: "seats", header: "Seats", render: (s) => seats(s.seatsTaken, s.capacity), muted: true },
            { key: "faculty", header: "Faculty", render: (s) => facultyNames(s), muted: true },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "a",
              header: "",
              render: (s) => (
                <Actions
                  disabled={busy}
                  actions={[
                    {
                      id: "section-capacity",
                      label: "Capacity",
                      onClick: () =>
                        ask({
                          title: `Capacity of section ${s.sectionCode}`,
                          name: "section-capacity",
                          fields: [{ name: "capacity", label: "Seats", type: "number", required: true, initial: String(s.capacity ?? "") }, REASON],
                          submitLabel: "Change capacity",
                          onSubmit: (v) => submit(() => api.changeCapacity(o.id, o.version, { sectionId: s.id, capacity: Number(v.capacity), reason: v.reason!.trim() }), "Capacity changed"),
                        }),
                    },
                    { id: "section-roster", label: roster === s.id ? "Hide roster" : "Roster", onClick: () => setRoster(roster === s.id ? null : s.id) },
                  ]}
                />
              ),
            },
          ]}
        />
      </BentoTile>
      {roster && (
        <BentoTile span={12} title="Roster" data-roster>
          <DataTable
            caption="Roster"
            rows={r.data ?? []}
            rowKey={(e) => e.id}
            empty={
              <EmptyState icon={Users} title="Nobody seated yet">
                Students seat themselves once their registration covers the course.
              </EmptyState>
            }
            columns={[
              { key: "n", header: "Student", render: (e) => <span data-roster-student={e.studentNumber}>{e.displayName}</span> },
              { key: "num", header: "Number", render: (e) => e.studentNumber, muted: true },
              { key: "origin", header: "Basis", render: (e) => roleLabel(e.origin), muted: true },
              { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
              {
                key: "a",
                header: "",
                render: (e) =>
                  e.status === "enrolled" ? (
                    <Actions
                      disabled={busy}
                      actions={[
                        {
                          id: "seat-withdraw",
                          label: "Withdraw",
                          onClick: () => ask({ title: `Withdraw ${e.displayName}`, name: "seat-withdraw", fields: [REASON], submitLabel: "Withdraw", onSubmit: (v) => submit(() => api.withdrawSeat(e.id, e.version, v.reason!.trim()), "Seat withdrawn") }),
                        },
                      ]}
                    />
                  ) : null,
              },
            ]}
          />
        </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: faculty assignment

export function FacultyAssignmentsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useOfferingApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const offerings = useQuery({ queryKey: okeys.scoped(institution?.id ?? "-", "offerings"), queryFn: () => api.listOfferings(institution!.id), enabled: !!institution });
  const policies = useQuery({ queryKey: okeys.scoped(institution?.id ?? "-", "faculty-policies"), queryFn: () => api.listPolicies(institution!.id), enabled: !!institution });
  if (!institution)
    return (
      <BentoTile span={12} data-slice="4A">
        <NoInstitution />
      </BentoTile>
    );
  const sections = (offerings.data ?? [])
    .filter((o) => !["archived", "cancelled", "merged", "split"].includes(o.status))
    .flatMap((o) => o.sections.filter((s) => s.status === "planned" || s.status === "active").map((s) => ({ o, s })));
  const facultyRows = sections.flatMap(({ o, s }) => s.faculty.map((f) => ({ o, s, f })));
  return (
    <>
      <BentoTile span={12} title="Teaching sections" data-slice="4A">
        <DataTable
          caption="Sections and their faculty"
          rows={sections}
          rowKey={(r) => r.s.id}
          empty={
            <EmptyState icon={Presentation} title="No sections to staff">
              Create and submit a course offering first.
            </EmptyState>
          }
          columns={[
            {
              key: "o",
              header: "Offering",
              render: (r) => (
                <span data-staff-section={`${r.o.offeringCode}/${r.s.sectionCode}`}>
                  {r.o.offeringCode} · {r.s.sectionCode}
                </span>
              ),
            },
            { key: "t", header: "Course", render: (r) => r.o.title, muted: true },
            { key: "f", header: "Faculty", render: (r) => facultyNames(r.s), muted: true },
            {
              key: "a",
              header: "",
              render: (r) => (
                <Actions
                  disabled={busy}
                  actions={[
                    {
                      id: "assignment-create",
                      label: "Assign faculty",
                      onClick: () =>
                        ask({
                          title: `Assign faculty to ${r.o.offeringCode} · ${r.s.sectionCode}`,
                          name: "assignment-create",
                          fields: [
                            { name: "facultySubjectRef", label: "Faculty login", required: true, placeholder: "dev|meera.shah@sutra.dev", wide: true },
                            { name: "facultyName", label: "Name", placeholder: "Dr. Meera Shah" },
                            { name: "role", label: "Role", type: "select", initial: "primary_instructor", options: ROLES.map((x) => ({ value: x, label: roleLabel(x) })) },
                            { name: "weeklyHours", label: "Weekly hours", type: "number" },
                          ],
                          submitLabel: "Create draft",
                          onSubmit: (v) =>
                            submit(
                              () =>
                                api.createAssignment(r.s.id, {
                                  facultySubjectRef: v.facultySubjectRef!.trim(),
                                  facultyName: v.facultyName?.trim() || null,
                                  role: (v.role || "primary_instructor") as FacultyRole,
                                  weeklyHours: v.weeklyHours ? Number(v.weeklyHours) : null,
                                }),
                              "Assignment drafted",
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
      <BentoTile span={12} title="Faculty assignments">
        <DataTable
          caption="Faculty assignments"
          rows={facultyRows}
          rowKey={(r) => r.f.assignmentId}
          empty={
            <EmptyState icon={GraduationCap} title="Nobody assigned yet">
              Assign a faculty member to each section; activation needs an approved assignment.
            </EmptyState>
          }
          columns={[
            { key: "w", header: "Faculty", render: (r) => <span data-assignment={r.f.facultySubjectRef}>{r.f.facultyName ?? r.f.facultySubjectRef}</span> },
            { key: "s", header: "Section", render: (r) => `${r.o.offeringCode} · ${r.s.sectionCode}`, muted: true },
            { key: "r", header: "Role", render: (r) => roleLabel(r.f.role), muted: true },
            {
              key: "st",
              header: "Status",
              render: (r) => (
                <span data-assignment-status={r.f.status}>
                  <StatusBadge status={r.f.status} />
                </span>
              ),
            },
            {
              key: "a",
              header: "",
              render: (r) => {
                const a = r.f;
                const version = async () => (await api.listAssignments(r.s.id)).find((x) => x.id === a.assignmentId)?.version ?? 0;
                const acts: RowAction[] = [];
                if (a.status === "draft")
                  acts.push({ id: "assignment-submit", label: "Submit", variant: "primary", onClick: () => void run(async () => api.assignmentCommand(a.assignmentId, await version(), "submit"), (x) => `Assignment ${x.status.replace(/_/g, " ")}`) });
                if (a.status === "submitted" || a.status === "under_review")
                  acts.push({ id: "assignment-approve", label: "Approve", variant: "primary", onClick: () => void run(async () => api.decideAssignment(a.assignmentId, await version(), "approve"), "Assignment approved") });
                if (a.status === "approved")
                  acts.push({ id: "assignment-activate", label: "Activate", variant: "primary", onClick: () => void run(async () => api.assignmentCommand(a.assignmentId, await version(), "activate"), "Assignment active") });
                if (a.status === "active")
                  acts.push({
                    id: "assignment-revoke",
                    label: "Revoke",
                    onClick: () =>
                      ask({ title: "Revoke assignment", name: "assignment-revoke", fields: [REASON], submitLabel: "Revoke", onSubmit: (v) => submit(async () => api.assignmentCommand(a.assignmentId, await version(), "revoke", v.reason!.trim()), "Assignment revoked") }),
                  });
                return <Actions disabled={busy} actions={acts} />;
              },
            },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Assignment governance"
        action={
          <Button
            size="sm"
            variant="ghost"
            icon={<ShieldCheck strokeWidth={1.75} />}
            data-action="policy-create"
            onClick={() =>
              ask({
                title: "Draft a governance version",
                name: "policy-create",
                fields: [
                  { name: "name", label: "Name", required: true, initial: "Workload norms" },
                  { name: "maxWeeklyHours", label: "Max weekly hours", type: "number", initial: "16" },
                  { name: "rationale", label: "Rationale", required: true, wide: true },
                ],
                submitLabel: "Create draft",
                onSubmit: (v) => submit(() => api.createPolicy(institution.id, { name: v.name!.trim(), maxWeeklyHours: v.maxWeeklyHours ? Number(v.maxWeeklyHours) : null, rationale: v.rationale!.trim() }), "Draft version created"),
              })
            }
          >
            New version
          </Button>
        }
      >
        <DataTable
          caption="Governance versions"
          rows={policies.data ?? []}
          rowKey={(p) => p.id}
          empty={
            <EmptyState icon={ShieldCheck} title="No governance configuration">
              Without one, any role is allowed and workload is not capped.
            </EmptyState>
          }
          columns={[
            {
              key: "v",
              header: "Version",
              render: (p) => (
                <span data-policy={p.versionNo}>
                  v{p.versionNo} · {p.name}
                </span>
              ),
            },
            { key: "h", header: "Workload cap", render: (p) => (p.maxWeeklyHours ? `${p.maxWeeklyHours} h/week` : "—"), muted: true },
            { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "a",
              header: "",
              render: (p) => {
                const acts: RowAction[] = [];
                if (p.status === "draft") acts.push({ id: "policy-submit", label: "Send to review", onClick: () => void run(() => api.submitPolicy(p.id, p.version), "Sent to review") });
                if (p.status === "in_review" && !p.reviewOutcome) {
                  acts.push({ id: "policy-compatible", label: "Review: compatible", variant: "primary", onClick: () => void run(() => api.reviewPolicy(p.id, p.version, "compatible"), (x) => `Version v${x.versionNo}: ${x.status.replace(/_/g, " ")}`) });
                  acts.push({
                    id: "policy-incompatible",
                    label: "Review: incompatible",
                    onClick: () =>
                      ask({
                        title: "Incompatible configuration",
                        name: "policy-incompatible",
                        fields: [{ name: "notes", label: "Review notes", required: true, wide: true }],
                        submitLabel: "Return to draft",
                        onSubmit: (v) => submit(() => api.reviewPolicy(p.id, p.version, "incompatible", v.notes!.trim()), "Returned to draft"),
                      }),
                  });
                }
                return <Actions disabled={busy} actions={acts} />;
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

// ================================================================== student: courses

export function StudentCoursesScreen() {
  const api = useOfferingApi();
  const { run, busy, dialog } = useCommand();
  const mine = useQuery({ queryKey: okeys.scoped("me", "courses"), queryFn: () => api.myCourses() });
  const open = useQuery({ queryKey: okeys.scoped("me", "available"), queryFn: () => api.availableCourses() });
  const courses = mine.data ?? [];
  const credits = courses.reduce((n, c) => n + c.credits, 0);
  const joinable = (open.data ?? []).filter((c) => !c.seated);
  return (
    <>
      <BentoTile span={12} variant="accent" data-slice="4A">
        <h1>My courses</h1>
        <p>The course sections you hold a seat in. Seats follow your registration: a course your registration covers can be joined below while its enrolment is open.</p>
      </BentoTile>
      <StatTile span={4} icon={BookOpen} tone="violet" label="Courses" value={String(courses.length)} />
      <StatTile span={4} icon={GraduationCap} tone="green" label="Credits" value={`${credits} cr`} />
      <StatTile span={4} icon={Layers} tone="blue" label="Open for enrolment" value={String(joinable.length)} />
      <BentoTile span={12} title="Enrolled">
        <DataTable
          caption="My courses"
          rows={courses}
          rowKey={(c) => c.enrolmentId}
          empty={
            <EmptyState icon={BookOpen} title="No courses yet">
              Once your registration is approved, take a seat in an open course below.
            </EmptyState>
          }
          columns={[
            {
              key: "c",
              header: "Course",
              render: (c) => (
                <span data-my-course={c.offeringCode}>
                  {c.offeringCode} · {c.title}
                </span>
              ),
            },
            { key: "s", header: "Section", render: (c) => c.sectionCode, muted: true },
            { key: "cr", header: "Credits", render: (c) => `${c.credits} cr`, muted: true },
            { key: "f", header: "Faculty", render: (c) => c.faculty.map((f) => f.facultyName ?? f.facultySubjectRef).join(", ") || "To be announced", muted: true },
            { key: "st", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            {
              key: "a",
              header: "",
              render: (c) =>
                c.status === "enrollment_open" ? <Actions disabled={busy} actions={[{ id: "course-drop", label: "Drop", onClick: () => void run(() => api.dropMine(c.enrolmentId, c.enrolmentVersion), `${c.offeringCode} dropped`) }]} /> : null,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Open for enrolment">
        <DataTable
          caption="Open courses"
          rows={joinable}
          rowKey={(c) => c.offeringId}
          empty={
            <EmptyState icon={Layers} title="Nothing open right now">
              Courses appear here when their enrolment window opens.
            </EmptyState>
          }
          columns={[
            {
              key: "c",
              header: "Course",
              render: (c) => (
                <span data-open-course={c.offeringCode}>
                  {c.offeringCode} · {c.title}
                </span>
              ),
            },
            { key: "cr", header: "Credits", render: (c) => `${c.credits} cr`, muted: true },
            { key: "cov", header: "Registration", render: (c) => (c.covered ? "Covered" : "Not on your registration"), muted: true },
            {
              key: "a",
              header: "",
              render: (c) => {
                if (!c.covered) return null;
                const free = c.sections.find((s) => !s.full);
                if (!free && c.waitlisted) return <span data-waitlisted>On the waitlist</span>;
                const acts: RowAction[] = free
                  ? [{ id: "course-enrol", label: `Enrol (section ${free.sectionCode})`, variant: "primary", onClick: () => void run(() => api.enrolMe(free.sectionId), `Enrolled in ${c.offeringCode}`) }]
                  : [{ id: "course-waitlist", label: "Join waitlist", onClick: () => void run(() => api.joinWaitlist(c.offeringId), (w) => `On the waitlist (position ${w.position})`) }];
                return <Actions disabled={busy} actions={acts} />;
              },
            },
          ]}
        />
      </BentoTile>
      {dialog}
    </>
  );
}

// ================================================================== faculty: classes / students

export function FacultyClassesScreen() {
  const api = useOfferingApi();
  const q = useQuery({ queryKey: okeys.scoped("my", "classes"), queryFn: () => api.myClasses() });
  const rows = q.data ?? [];
  return (
    <>
      <BentoTile span={12} variant="accent" data-slice="4A">
        <h1>My classes</h1>
        <p>The sections you actively teach this term. Attendance, coursework and resources open from each class.</p>
      </BentoTile>
      <StatTile span={6} icon={Presentation} tone="violet" label="Sections" value={String(rows.length)} />
      <StatTile span={6} icon={Users} tone="teal" label="Students" value={String(rows.reduce((n, c) => n + c.seatedCount, 0))} />
      <BentoTile span={12} title="Sections I teach">
        <DataTable
          caption="My classes"
          rows={rows}
          rowKey={(c) => c.sectionId}
          empty={
            <EmptyState icon={Presentation} title="No active teaching assignment">
              Your classes appear once an administrator activates your assignment.
            </EmptyState>
          }
          columns={[
            {
              key: "c",
              header: "Class",
              render: (c) => (
                <span data-my-class={`${c.offeringCode}/${c.sectionCode}`}>
                  {c.offeringCode} · {c.title}
                </span>
              ),
            },
            { key: "s", header: "Section", render: (c) => c.sectionCode, muted: true },
            { key: "r", header: "Role", render: (c) => roleLabel(c.role), muted: true },
            { key: "n", header: "Students", render: (c) => c.seatedCount, muted: true },
            { key: "st", header: "Status", render: (c) => <StatusBadge status={c.offeringStatus} /> },
          ]}
        />
      </BentoTile>
    </>
  );
}

export function FacultyStudentsScreen() {
  const api = useOfferingApi();
  const q = useQuery({ queryKey: okeys.scoped("my", "students"), queryFn: () => api.myStudents() });
  const rows = q.data ?? [];
  return (
    <>
      <BentoTile span={12} variant="accent" data-slice="4A">
        <h1>My students</h1>
        <p>Students seated in the sections you teach. Open a student for the read-only Student 360.</p>
      </BentoTile>
      <BentoTile span={12} title="Rosters">
        <DataTable
          caption="My students"
          rows={rows}
          rowKey={(s) => `${s.sectionId}:${s.studentId}`}
          empty={
            <EmptyState icon={Users} title="No students yet">
              Students appear once they take a seat in one of your sections.
            </EmptyState>
          }
          columns={[
            {
              key: "n",
              header: "Student",
              render: (s) => (
                <Link to={`/students/record/${s.studentId}`} data-my-student={s.studentNumber}>
                  {s.displayName}
                </Link>
              ),
            },
            { key: "num", header: "Number", render: (s) => s.studentNumber, muted: true },
            { key: "c", header: "Class", render: (s) => `${s.offeringCode} · ${s.sectionCode}`, muted: true },
          ]}
        />
      </BentoTile>
    </>
  );
}
