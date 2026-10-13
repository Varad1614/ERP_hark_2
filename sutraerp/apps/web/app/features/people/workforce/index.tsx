/**
 * Slice **7A** web components (staff records, engagements, positions, recruitment → onboarding,
 * My HR record). Export names are imported by the foundation frames (`routes/admin/people.tsx`,
 * `routes/faculty/leave.tsx`); every screen carries `data-slice="7A"` and uses @sutra/ui only.
 *
 * - `StaffAdminTab` — Admin → People → Staff: list, create, verification, drawer with engagements
 *   (create, submit / decide / activate, suspend / restore, close).
 * - `RecruitmentAdminTab` — requisitions, candidates, onboarding cases (documents → joining).
 * - `PositionsAdminTab` — positions and their assignments.
 * - `MyProfilePanel` — My HR (span 4): own record, engagement, qualifications, certificates.
 */
import { Award, BadgeCheck, Briefcase, ClipboardList, GraduationCap, IdCard, ShieldCheck, UserPlus, Users } from "lucide-react";
import { BentoTile, Button, DataTable, Drawer, EmptyState, Select, StatTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, NoInstitution, REASON, todayIso, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { useWorkforceApi, wkeys } from "./hooks";
import type { Candidate, Engagement, EngagementCommand, OnboardingCase, Position, Requisition, Staff, StaffCommand } from "./types";

export { WorkforceApiProvider } from "./hooks";

const dash = (v: string | null | undefined) => v ?? "—";

const CATEGORY = [
  { value: "teaching", label: "Teaching" },
  { value: "non_teaching", label: "Non-teaching" },
  { value: "administrative", label: "Administrative" },
  { value: "technical", label: "Technical" },
  { value: "support", label: "Support" },
];
const EMPLOYMENT = [
  { value: "permanent", label: "Permanent" },
  { value: "probation", label: "Probation" },
  { value: "contract", label: "Contract" },
  { value: "visiting", label: "Visiting" },
  { value: "part_time", label: "Part-time" },
];
const STAFF_STATUS = ["", "draft", "pending_verification", "verified", "active", "suspended", "archived"];

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="7A">
      <NoInstitution />
    </BentoTile>
  );
}

/** Next verification / lifecycle step of a staff record. */
export function staffActions(s: Pick<Staff, "status">): StaffCommand[] {
  switch (s.status) {
    case "draft":
      return ["submit-verification"];
    case "pending_verification":
      return ["verify"];
    case "verified":
      return ["activate"];
    case "active":
      return ["suspend"];
    case "suspended":
      return ["restore", "archive"];
    default:
      return [];
  }
}

/** Commands offered for an engagement (governed decisions included). */
export function engagementActions(e: Pick<Engagement, "status" | "pendingChange">): Array<EngagementCommand | "approve" | "reject" | "close"> {
  const pc = e.pendingChange;
  if (pc?.status === "pending_approval") return ["approve", "reject"];
  if (pc?.status === "approved" && pc.kind === "activation") return ["activate"];
  switch (e.status) {
    case "pending":
      return ["submit", "cancel"];
    case "active":
      return ["suspend", "close"];
    case "suspended":
    case "long_leave":
    case "deputed":
      return e.status === "suspended" ? ["restore", "close"] : ["close"];
    default:
      return [];
  }
}

// ================================================================================= staff

/** Admin → People → Staff records. */
export function StaffAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <StaffAdmin institution={institution} />;
}

function StaffAdmin({ institution }: { institution: Institution }) {
  const api = useWorkforceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [status, setStatus] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const list = useQuery({ queryKey: wkeys.scoped(institution.id, `staff:${status}`), queryFn: () => api.staff(institution.id, status || undefined) });

  const create = () =>
    ask({
      title: "New staff record",
      name: "staff-create",
      submitLabel: "Create",
      fields: [
        { name: "givenName", label: "Given name", required: true },
        { name: "familyName", label: "Family name" },
        { name: "workEmail", label: "Work e-mail", placeholder: "name@college.edu" },
        { name: "staffCategory", label: "Category", type: "select", options: CATEGORY, initial: "teaching", required: true },
        { name: "subjectRef", label: "Sign-in account (optional)", placeholder: "dev|name@college.edu", wide: true },
      ],
      onSubmit: (v) =>
        submit(
          () => api.createStaff(institution.id, { givenName: v.givenName!, familyName: v.familyName || undefined, workEmail: v.workEmail || undefined, staffCategory: v.staffCategory!, subjectRef: v.subjectRef?.trim() || undefined }),
          (s) => `${s.staffNumber} created (draft)`,
        ),
    });

  const onStaff = (s: Staff, cmd: StaffCommand) => {
    if (cmd === "suspend" || cmd === "archive")
      return ask({ title: `${statusLabel(cmd)} · ${s.staffNumber}`, name: `staff-${cmd}`, submitLabel: statusLabel(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.staffCommand(s.id, s.version, cmd, v.reason), `${s.staffNumber}: ${statusLabel(cmd)}`) });
    return run(() => api.staffCommand(s.id, s.version, cmd), `${s.staffNumber}: ${statusLabel(cmd)}`);
  };

  const all = useQuery({ queryKey: wkeys.scoped(institution.id, "staff:"), queryFn: () => api.staff(institution.id) }).data;
  const count = (f: (s: Staff) => boolean) => (all ? String(all.filter(f).length) : "…");
  return (
    <>
      <StatTile icon={Users} tone="orange" label="Staff records" value={all ? String(all.length) : "…"} hint="All categories" data-kpi="staff-total" />
      <StatTile icon={GraduationCap} tone="green" label="Faculty on roll" value={count((s) => s.staffCategory === "teaching" && s.engagement?.status === "active")} hint="Teaching, engagement active" data-kpi="staff-faculty" />
      <StatTile icon={ShieldCheck} tone="violet" label="To verify" value={count((s) => s.status === "pending_verification")} hint="Records awaiting verification" data-kpi="staff-verify" />
      <StatTile icon={Briefcase} tone="blue" label="Engagements pending" value={count((s) => s.engagement?.status === "pending")} hint="Awaiting activation" data-kpi="staff-pending" />
      <BentoTile
        span={12}
        title="Staff records"
        data-slice="7A"
        data-staff
        action={
          <Button size="sm" variant="ghost" data-action="staff-new" disabled={busy} onClick={create}>
            New staff
          </Button>
        }
      >
        <Select label="Status" value={status} onChange={(e) => setStatus(e.target.value)} options={STAFF_STATUS.map((s) => ({ value: s, label: s ? statusLabel(s) : "All" }))} data-field="staff-status" />
        <DataTable
          caption="Staff"
          rows={list.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={Users} title="No staff records">Create a record or onboard a candidate from Recruitment.</EmptyState>}
          columns={[
            { key: "number", header: "Staff no.", render: (s) => s.staffNumber },
            { key: "name", header: "Name", render: (s) => s.displayName },
            { key: "category", header: "Category", render: (s) => statusLabel(s.staffCategory), muted: true },
            { key: "engagement", header: "Engagement", render: (s) => (s.engagement ? `${s.engagement.designation} · ${statusLabel(s.engagement.status)}` : "—") },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "actions",
              header: "",
              render: (s) => (
                <Actions
                  disabled={busy}
                  actions={[
                    ...staffActions(s).map((c): RowAction => ({ id: `staff-${c}`, label: statusLabel(c), variant: c === "verify" || c === "activate" ? "primary" : "ghost", onClick: () => void onStaff(s, c) })),
                    { id: "staff-open", label: "Open", onClick: () => setOpen(s.id) },
                  ]}
                />
              ),
            },
          ]}
        />
      </BentoTile>
      {open ? <StaffDrawer staffId={open} onClose={() => setOpen(null)} /> : null}
      {prompt}
      {dialog}
    </>
  );
}

function StaffDrawer({ staffId, onClose }: { staffId: string; onClose: () => void }) {
  const api = useWorkforceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const record = useQuery({ queryKey: wkeys.scoped(staffId, "record"), queryFn: () => api.staffRecord(staffId) });
  const engs = useQuery({ queryKey: wkeys.scoped(staffId, "engagements"), queryFn: () => api.engagements(staffId) });
  const s = record.data;
  const colleagues = useQuery({ queryKey: wkeys.scoped(s?.institutionId ?? "-", "staff:"), queryFn: () => api.staff(s!.institutionId), enabled: !!s }).data ?? [];
  const managers = colleagues.filter((c) => c.id !== staffId && c.engagement && !["closed", "cancelled"].includes(c.engagement.status));
  const nameOf = (id?: string | null) => (id ? (colleagues.find((c) => c.id === id)?.displayName ?? "—") : "—");
  const reporting = (e: Engagement) =>
    ask({
      title: `Reporting manager · ${e.engagementNumber}`,
      name: "engagement-reporting",
      submitLabel: "Save",
      fields: [
        { name: "manager", label: "Reports to", type: "select", options: [{ value: "", label: "No reporting manager" }, ...managers.map((m) => ({ value: m.id, label: `${m.staffNumber} · ${m.displayName}` }))], initial: e.reportsToStaffId ?? managers[0]?.id ?? "" },
        { name: "reason", label: "Reason", wide: true },
      ],
      onSubmit: (v) => submit(() => api.setReporting(e.id, e.version, v.manager || null, v.reason || undefined), (r) => `${r.engagementNumber} reports to ${nameOf(r.reportsToStaffId)}`),
    });

  const newEngagement = () =>
    ask({
      title: "New engagement",
      name: "engagement-create",
      submitLabel: "Create",
      fields: [
        { name: "employmentType", label: "Employment type", type: "select", options: EMPLOYMENT, initial: "permanent", required: true },
        { name: "designation", label: "Designation", required: true },
        { name: "grade", label: "Grade", placeholder: "AP-1" },
        { name: "startDate", label: "Start date", type: "date", required: true, initial: todayIso() },
        { name: "probationEndDate", label: "Probation ends", type: "date" },
      ],
      onSubmit: (v) =>
        submit(
          () => api.createEngagement(staffId, { employmentType: v.employmentType!, designation: v.designation!, grade: v.grade || undefined, startDate: v.startDate!, probationEndDate: v.probationEndDate || undefined }),
          (e) => `${e.engagementNumber} created (pending)`,
        ),
    });

  const onEngagement = (e: Engagement, a: ReturnType<typeof engagementActions>[number]) => {
    if (a === "approve") return run(() => api.decideEngagement(e.id, e.version, "approve"), `${e.engagementNumber} approved`);
    if (a === "reject") return ask({ title: `Reject · ${e.engagementNumber}`, name: "engagement-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideEngagement(e.id, e.version, "reject", v.reason), `${e.engagementNumber} sent back`) });
    if (a === "close")
      return ask({
        title: `Close · ${e.engagementNumber}`,
        name: "engagement-close",
        submitLabel: "Close engagement",
        fields: [{ name: "lastWorkingDate", label: "Last working date", type: "date", required: true, initial: todayIso() }, REASON],
        onSubmit: (v) => submit(() => api.closeEngagement(e.id, e.version, v.lastWorkingDate!, v.reason!), `${e.engagementNumber} closed`),
      });
    const cmd = a;
    if (cmd === "suspend" || cmd === "cancel")
      return ask({ title: `${statusLabel(cmd)} · ${e.engagementNumber}`, name: `engagement-${cmd}`, submitLabel: statusLabel(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.engagementCommand(e.id, e.version, cmd, v.reason), `${e.engagementNumber}: ${statusLabel(cmd)}`) });
    return run(() => api.engagementCommand(e.id, e.version, cmd), `${e.engagementNumber}: ${statusLabel(cmd)}`);
  };

  return (
    <Drawer open onClose={onClose} title={s ? s.displayName : "Staff record"} eyebrow={s?.staffNumber}>
      <div data-slice="7A" data-staff-drawer>
        {s ? (
          <dl className="sb-dl">
            <dt>Status</dt>
            <dd>
              <StatusBadge status={s.status} />
            </dd>
            <dt>Category</dt>
            <dd>{statusLabel(s.staffCategory)}</dd>
            <dt>Work e-mail</dt>
            <dd>{dash(s.workEmail)}</dd>
            <dt>Login</dt>
            <dd>{dash(s.subjectRef)}</dd>
          </dl>
        ) : null}
        <Button size="sm" variant="ghost" data-action="engagement-new" disabled={busy} onClick={newEngagement}>
          New engagement
        </Button>
        <DataTable
          caption="Engagements"
          rows={engs.data ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState icon={Briefcase} title="No engagements">Create one, then submit it for activation.</EmptyState>}
          columns={[
            {
              key: "engagement",
              header: "Engagement",
              render: (e) => (
                <>
                  <b>{`${e.designation}${e.grade ? ` (${e.grade})` : ""}`}</b>
                  <br />
                  {`${e.engagementNumber} · ${statusLabel(e.employmentType)}`}
                  <br />
                  {(e.lastWorkingDate ?? e.endDate) ? `${fmtDate(e.startDate)} – ${fmtDate(e.lastWorkingDate ?? e.endDate)}` : `Since ${fmtDate(e.startDate)}`}
                  {e.reportsToStaffId ? ` · reports to ${nameOf(e.reportsToStaffId)}` : ""}
                </>
              ),
            },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.pendingChange ? e.pendingChange.status : e.status} /> },
            {
              key: "actions",
              header: "",
              render: (e) => (
                <Actions
                  disabled={busy}
                  actions={[
                    ...engagementActions(e).map((a): RowAction => ({ id: `engagement-${a}`, label: statusLabel(a), variant: a === "submit" || a === "approve" ? "primary" : "ghost", onClick: () => void onEngagement(e, a) })),
                    ...(["pending", "active", "suspended", "long_leave", "deputed"].includes(e.status) ? [{ id: "engagement-reporting", label: "Reporting", onClick: () => reporting(e) }] : []),
                  ]}
                />
              ),
            },
          ]}
        />
      </div>
      {prompt}
      {dialog}
    </Drawer>
  );
}

// ================================================================================= recruitment

function requisitionActions(r: Requisition): Array<"submit" | "approve" | "reject" | "close"> {
  switch (r.status) {
    case "draft":
      return ["submit", "close"];
    case "pending_approval":
      return ["approve", "reject"];
    case "approved":
    case "open":
      return ["close"];
    default:
      return [];
  }
}

function candidateActions(c: Candidate): Array<"shortlist" | "interview" | "offer" | "accept" | "decline" | "reject" | "onboard"> {
  switch (c.status) {
    case "applied":
      return ["shortlist", "reject"];
    case "shortlisted":
      return ["interview", "offer", "reject"];
    case "interviewed":
      return ["offer", "reject"];
    case "offered":
      return ["accept", "decline"];
    case "offer_accepted":
      return c.onboardingCaseId ? [] : ["onboard"];
    default:
      return [];
  }
}

function onboardingActions(o: OnboardingCase): Array<"documents" | "verify" | "return" | "ready" | "join"> {
  switch (o.status) {
    case "initiated":
    case "documentation_pending":
      return ["documents"];
    case "verification_pending":
      return o.documentsVerified ? ["ready"] : ["verify", "return"];
    case "ready_for_joining":
      return o.processInstanceId ? [] : ["join"];
    default:
      return [];
  }
}

/** Admin → People → Recruitment & onboarding. */
export function RecruitmentAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <RecruitmentAdmin institution={institution} />;
}

function RecruitmentAdmin({ institution }: { institution: Institution }) {
  const api = useWorkforceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const reqs = useQuery({ queryKey: wkeys.scoped(institution.id, "requisitions"), queryFn: () => api.requisitions(institution.id) });
  const cases = useQuery({ queryKey: wkeys.scoped(institution.id, "onboarding"), queryFn: () => api.onboardingCases(institution.id) });
  const [picked, setPicked] = useState("");
  const reqList = reqs.data ?? [];
  const reqId = picked || reqList.find((r) => r.status === "open")?.id || reqList[0]?.id || "";
  const cands = useQuery({ queryKey: wkeys.scoped(reqId, "candidates"), queryFn: () => api.candidates(reqId), enabled: reqId !== "" });

  const newRequisition = () =>
    ask({
      title: "New requisition",
      name: "requisition-create",
      submitLabel: "Create",
      fields: [
        { name: "title", label: "Title", required: true, wide: true },
        { name: "designation", label: "Designation", required: true },
        { name: "staffCategory", label: "Category", type: "select", options: CATEGORY, initial: "teaching", required: true },
        { name: "employmentType", label: "Employment type", type: "select", options: EMPLOYMENT, initial: "probation", required: true },
        { name: "openings", label: "Openings", type: "number", initial: "1", required: true },
      ],
      onSubmit: (v) =>
        submit(
          () => api.createRequisition(institution.id, { title: v.title!, designation: v.designation!, staffCategory: v.staffCategory!, employmentType: v.employmentType!, openings: Number(v.openings) }),
          (r) => `${r.number} created`,
        ),
    });

  const onRequisition = (r: Requisition, a: ReturnType<typeof requisitionActions>[number]) => {
    if (a === "submit") return run(() => api.submitRequisition(r.id, r.version), `${r.number} submitted`);
    if (a === "approve") return run(() => api.decideRequisition(r.id, r.version, "approve"), `${r.number} approved`);
    if (a === "reject") return ask({ title: `Reject · ${r.number}`, name: "requisition-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideRequisition(r.id, r.version, "reject", v.reason), `${r.number} sent back`) });
    return ask({ title: `Close · ${r.number}`, name: "requisition-close", submitLabel: "Close", fields: [REASON], onSubmit: (v) => submit(() => api.closeRequisition(r.id, r.version, v.reason!), `${r.number} closed`) });
  };

  const addCandidate = () =>
    ask({
      title: "Add candidate",
      name: "candidate-create",
      submitLabel: "Add",
      fields: [
        { name: "givenName", label: "Given name", required: true },
        { name: "familyName", label: "Family name" },
        { name: "email", label: "E-mail", required: true },
      ],
      onSubmit: (v) => submit(() => api.addCandidate(reqId, { givenName: v.givenName!, familyName: v.familyName || undefined, email: v.email! }), "Candidate added"),
    });

  const onCandidate = (c: Candidate, a: ReturnType<typeof candidateActions>[number]) => {
    if (a === "offer")
      return ask({ title: `Offer · ${c.givenName}`, name: "candidate-offer", submitLabel: "Make offer", fields: [{ name: "joining", label: "Proposed joining date", type: "date", required: true, initial: todayIso() }], onSubmit: (v) => submit(() => api.offer(c.id, c.version, v.joining!), "Offer made") });
    if (a === "onboard") {
      const fields: FieldSpec[] = [{ name: "probationEndDate", label: "Probation ends (probation hires)", type: "date" }];
      return ask({ title: `Onboard · ${c.givenName}`, name: "onboarding-create", submitLabel: "Start onboarding", fields, onSubmit: (v) => submit(() => api.startOnboarding(c.id, { probationEndDate: v.probationEndDate || undefined }), (o) => `${o.number} opened`) });
    }
    const cmd = a;
    if (cmd === "decline" || cmd === "reject") return ask({ title: `${statusLabel(cmd)} · ${c.givenName}`, name: `candidate-${cmd}`, submitLabel: statusLabel(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.candidateCommand(c.id, c.version, cmd, v.reason), statusLabel(cmd)) });
    return run(() => api.candidateCommand(c.id, c.version, cmd), `${c.givenName}: ${statusLabel(cmd)}`);
  };

  const onCase = (o: OnboardingCase, a: ReturnType<typeof onboardingActions>[number]) => {
    if (a === "documents")
      return ask({
        title: `Documents · ${o.number}`,
        name: "onboarding-documents",
        submitLabel: "Submit for verification",
        fields: [
          { name: "documentType", label: "Document", type: "select", options: ["identity", "address", "education", "employment", "medical", "other"].map((d) => ({ value: d, label: statusLabel(d) })), initial: "identity", required: true },
          { name: "reference", label: "Reference", required: true },
        ],
        onSubmit: (v) => submit(() => api.onboardingDocuments(o.id, o.version, v.documentType!, v.reference!), "Documents submitted"),
      });
    if (a === "verify") return run(() => api.verifyOnboarding(o.id, o.version, true), "Documents verified");
    if (a === "return") return ask({ title: `Return · ${o.number}`, name: "onboarding-return", submitLabel: "Return", fields: [REASON], onSubmit: (v) => submit(() => api.verifyOnboarding(o.id, o.version, false, v.reason), "Returned for documents") });
    if (a === "ready") return run(() => api.onboardingReady(o.id, o.version), `${o.number} ready for joining`);
    return run(() => api.join(o.id, o.version), (r) => (r.status === "joined" ? `${o.number}: joined` : `${o.number}: awaiting acceptance`));
  };

  return (
    <>
      <BentoTile span={12} title="Requisitions" data-slice="7A" data-requisitions action={<Button size="sm" variant="ghost" data-action="requisition-new" disabled={busy} onClick={newRequisition}>New requisition</Button>}>
        <DataTable
          caption="Requisitions"
          rows={reqList}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={ClipboardList} title="No requisitions">Raise one to start hiring.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (r) => r.number },
            { key: "title", header: "Title", render: (r) => r.title },
            { key: "openings", header: "Filled", render: (r) => `${r.filled} / ${r.openings}` },
            { key: "type", header: "Hire as", render: (r) => `${statusLabel(r.staffCategory)} · ${statusLabel(r.employmentType)}`, muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions disabled={busy} actions={requisitionActions(r).map((a) => ({ id: `requisition-${a}`, label: statusLabel(a), variant: a === "submit" || a === "approve" ? "primary" : "ghost", onClick: () => void onRequisition(r, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Candidates" data-slice="7A" data-candidates action={reqId ? <Button size="sm" variant="ghost" data-action="candidate-new" disabled={busy} onClick={addCandidate}>Add candidate</Button> : undefined}>
        <Select label="Requisition" value={reqId} onChange={(e) => setPicked(e.target.value)} options={reqList.map((r) => ({ value: r.id, label: `${r.number} · ${r.title}` }))} data-field="requisition" />
        <DataTable
          caption="Candidates"
          rows={cands.data ?? []}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={UserPlus} title="No candidates">Add applicants to an open requisition.</EmptyState>}
          columns={[
            { key: "name", header: "Candidate", render: (c) => [c.givenName, c.familyName].filter(Boolean).join(" ") },
            { key: "email", header: "E-mail", render: (c) => c.email, muted: true },
            { key: "offer", header: "Offer", render: (c) => (c.proposedJoiningDate ? `${dash(c.offeredDesignation)} · joins ${fmtDate(c.proposedJoiningDate)}` : "—") },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            { key: "actions", header: "", render: (c) => <Actions disabled={busy} actions={candidateActions(c).map((a) => ({ id: `candidate-${a}`, label: a === "onboard" ? "Start onboarding" : statusLabel(a), variant: a === "accept" || a === "onboard" ? "primary" : "ghost", onClick: () => void onCandidate(c, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Onboarding" data-slice="7A" data-onboarding>
        <DataTable
          caption="Onboarding cases"
          rows={cases.data ?? []}
          rowKey={(o) => o.id}
          empty={<EmptyState icon={IdCard} title="No onboarding cases">Start one from a candidate who accepted an offer.</EmptyState>}
          columns={[
            { key: "number", header: "Case", render: (o) => o.number },
            { key: "name", header: "Joiner", render: (o) => o.candidateName },
            { key: "joining", header: "Joining", render: (o) => fmtDate(o.joiningDate) },
            { key: "docs", header: "Documents", render: (o) => `${o.documents.length}${o.documentsVerified ? " · verified" : ""}`, muted: true },
            { key: "status", header: "Status", render: (o) => <StatusBadge status={o.processInstanceId && o.status === "ready_for_joining" ? "pending_approval" : o.status} /> },
            { key: "actions", header: "", render: (o) => <Actions disabled={busy} actions={onboardingActions(o).map((a) => ({ id: `onboarding-${a}`, label: statusLabel(a), variant: a === "join" ? "primary" : "ghost", onClick: () => void onCase(o, a) }))} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================================= positions

function positionActions(p: Position): Array<"activate" | "freeze" | "retire"> {
  switch (p.status) {
    case "draft":
      return ["activate"];
    case "active":
      return ["freeze", "retire"];
    case "frozen":
      return ["activate", "retire"];
    default:
      return [];
  }
}

/** Admin → People → Positions & reporting. */
export function PositionsAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <PositionsAdmin institution={institution} />;
}

function PositionsAdmin({ institution }: { institution: Institution }) {
  const api = useWorkforceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const positions = useQuery({ queryKey: wkeys.scoped(institution.id, "positions"), queryFn: () => api.positions(institution.id) });
  const active = useQuery({ queryKey: wkeys.scoped(institution.id, "staff:active"), queryFn: () => api.staff(institution.id, "active") });
  const [picked, setPicked] = useState("");
  const list = positions.data ?? [];
  const posId = picked || list[0]?.id || "";
  const assignments = useQuery({ queryKey: wkeys.scoped(posId, "assignments"), queryFn: () => api.assignments(posId), enabled: posId !== "" });
  const engaged = (active.data ?? []).filter((s) => s.engagement?.status === "active");

  const newPosition = () =>
    ask({
      title: "New position",
      name: "position-create",
      submitLabel: "Create",
      fields: [
        { name: "code", label: "Code", required: true, placeholder: "PROF-CS-01" },
        { name: "title", label: "Title", required: true },
        { name: "staffCategory", label: "Category", type: "select", options: [{ value: "", label: "Any" }, ...CATEGORY] },
        { name: "allowsMultiple", label: "Several occupants", type: "checkbox" },
      ],
      onSubmit: (v) => submit(() => api.createPosition(institution.id, { code: v.code!, title: v.title!, staffCategory: v.staffCategory || undefined, allowsMultiple: v.allowsMultiple === "true" }), (p) => `${p.code} created (draft)`),
    });

  const onPosition = (p: Position, a: ReturnType<typeof positionActions>[number]) => {
    if (a === "activate") return run(() => api.positionCommand(p.id, p.version, a), `${p.code} active`);
    return ask({ title: `${statusLabel(a)} · ${p.code}`, name: `position-${a}`, submitLabel: statusLabel(a), fields: [REASON], onSubmit: (v) => submit(() => api.positionCommand(p.id, p.version, a, v.reason), `${p.code}: ${statusLabel(a)}`) });
  };

  const assign = () =>
    ask({
      title: "Assign to position",
      name: "position-assign",
      submitLabel: "Assign",
      fields: [
        { name: "engagementId", label: "Staff member", type: "select", required: true, options: engaged.map((s) => ({ value: s.engagement?.id ?? "", label: `${s.staffNumber} · ${s.displayName}` })), initial: engaged[0]?.engagement?.id },
        { name: "startDate", label: "From", type: "date", required: true, initial: todayIso() },
      ],
      onSubmit: (v) => submit(() => api.assign(posId, { engagementId: v.engagementId!, startDate: v.startDate! }), (a) => (a.status === "active" ? "Assigned" : "Assignment awaits approval")),
    });

  return (
    <>
      <BentoTile span={12} title="Positions" data-slice="7A" data-positions action={<Button size="sm" variant="ghost" data-action="position-new" disabled={busy} onClick={newPosition}>New position</Button>}>
        <DataTable
          caption="Positions"
          rows={list}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={Award} title="No positions">Define the institution's positions, then assign staff.</EmptyState>}
          columns={[
            { key: "code", header: "Code", render: (p) => p.code },
            { key: "title", header: "Title", render: (p) => p.title },
            { key: "occupants", header: "Occupants", render: (p) => `${p.occupants}${p.allowsMultiple ? "" : " / 1"}`, muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "actions", header: "", render: (p) => <Actions disabled={busy} actions={positionActions(p).map((a) => ({ id: `position-${a}`, label: statusLabel(a), onClick: () => void onPosition(p, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Assignments" data-slice="7A" data-assignments action={posId ? <Button size="sm" variant="ghost" data-action="assignment-new" disabled={busy || engaged.length === 0} onClick={assign}>Assign</Button> : undefined}>
        <Select label="Position" value={posId} onChange={(e) => setPicked(e.target.value)} options={list.map((p) => ({ value: p.id, label: `${p.code} · ${p.title}` }))} data-field="position" />
        <DataTable
          caption="Assignments"
          rows={assignments.data ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState icon={Briefcase} title="No assignments">Assign an active engagement to this position.</EmptyState>}
          columns={[
            { key: "staff", header: "Staff", render: (a) => `${a.staffNumber} · ${a.displayName}` },
            { key: "period", header: "Period", render: (a) => `${fmtDate(a.startDate)} – ${fmtDate(a.endDate)}`, muted: true },
            { key: "outcome", header: "Ended", render: (a) => (a.endOutcome ? statusLabel(a.endOutcome) : "—"), muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
            {
              key: "actions",
              header: "",
              render: (a) => {
                const acts: RowAction[] = [];
                if (a.status === "pending_approval") {
                  acts.push({ id: "assignment-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideAssignment(a.id, a.version, "approve"), "Assignment approved") });
                  acts.push({ id: "assignment-reject", label: "Reject", onClick: () => ask({ title: "Reject assignment", name: "assignment-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideAssignment(a.id, a.version, "reject", v.reason), "Assignment rejected") }) });
                }
                if (a.status === "active")
                  acts.push({
                    id: "assignment-end",
                    label: "End",
                    onClick: () =>
                      ask({
                        title: `End · ${a.staffNumber}`,
                        name: "assignment-end",
                        submitLabel: "End assignment",
                        fields: [
                          { name: "endDate", label: "Last day", type: "date", required: true, initial: todayIso() },
                          { name: "outcome", label: "Outcome", type: "select", options: [{ value: "completed", label: "Completed" }, { value: "terminated", label: "Terminated" }], initial: "completed", required: true },
                          REASON,
                        ],
                        onSubmit: (v) => submit(() => api.endAssignment(a.id, a.version, { endDate: v.endDate!, outcome: v.outcome === "terminated" ? "terminated" : "completed", reason: v.reason! }), "Assignment ended"),
                      }),
                  });
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

// ================================================================================= My HR

/** My HR (`/leave`, span 4): own staff record, engagement, qualifications and certificates. */
export function MyProfilePanel() {
  const api = useWorkforceApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const me = useQuery({ queryKey: wkeys.scoped("me", "profile"), queryFn: () => api.myProfile() });
  const p = me.data?.staff ? me.data : null;
  const staff = p?.staff;

  const request = () =>
    ask({
      title: "Request a certificate",
      name: "certificate-request",
      submitLabel: "Request",
      fields: [
        { name: "kind", label: "Certificate", type: "select", options: [{ value: "service", label: "Service" }, { value: "experience", label: "Experience" }], initial: "service", required: true },
        { name: "purpose", label: "Purpose", required: true, wide: true },
      ],
      onSubmit: (v) => submit(() => api.requestCertificate({ kind: v.kind === "experience" ? "experience" : "service", purpose: v.purpose! }), "Certificate requested"),
    });

  return (
    <BentoTile
      span={12}
      title="My record"
      data-slice="7A"
      data-my-profile
      action={
        p ? (
          <Button size="sm" variant="ghost" data-action="certificate-request" disabled={busy} onClick={request}>
            Request certificate
          </Button>
        ) : undefined
      }
    >
      {p && staff ? (
        <>
          <p>
            <strong>{staff.displayName}</strong> · {staff.staffNumber}
          </p>
          <p>
            {p.engagement ? `${p.engagement.designation} · ${statusLabel(p.engagement.employmentType)} since ${fmtDate(p.engagement.startDate)}` : "No engagement"}{" "}
            <StatusBadge status={p.engagement?.status ?? staff.status} />
          </p>
          {p.faculty?.eligibilityStatus ? (
            <p>
              Teaching eligibility <StatusBadge status={p.faculty.eligibilityStatus} />
            </p>
          ) : null}
          <DataTable
            caption="Qualifications"
            rows={p.qualifications}
            rowKey={(q) => q.id}
            empty={<EmptyState icon={BadgeCheck} title="No qualifications">HR records them on your profile.</EmptyState>}
            columns={[
              { key: "title", header: "Qualification", render: (q) => `${q.title}${q.yearOfCompletion ? ` (${q.yearOfCompletion})` : ""}` },
              { key: "status", header: "", render: (q) => <StatusBadge status={q.status} /> },
            ]}
          />
          <DataTable
            caption="Certificate requests"
            rows={p.certificateRequests}
            rowKey={(c) => c.id}
            empty={<EmptyState icon={Award} title="No certificate requests">Ask for a service or experience certificate.</EmptyState>}
            columns={[
              { key: "kind", header: "Certificate", render: (c) => `${statusLabel(c.kind)} · ${c.purpose}` },
              { key: "status", header: "", render: (c) => (c.certificateNumber ? c.certificateNumber : <StatusBadge status={c.status} />) },
            ]}
          />
        </>
      ) : (
        <EmptyState icon={IdCard} title="No staff record">{me.isLoading ? "Loading…" : "Your login is not linked to a staff record yet."}</EmptyState>
      )}
      {prompt}
      {dialog}
    </BentoTile>
  );
}
