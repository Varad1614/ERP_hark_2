/**
 * Slice **3C** web components (completion & exits, guardians, scholarships, alumni).
 * The routes import these export names (foundation wiring, docs/batch3-contracts.md §10).
 */
import { Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router";
import { Actions, fmtDate, NoInstitution, QuickForm, REASON, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import type { CompletionCommand, EligibilityCommand, RelationshipCommand, ScholarshipCommand } from "./api";
import { k3, useC3Api } from "./hooks";
import { student360Path } from "../index";
// The record command runner also refreshes the Student 360 header/overview (3A queries) after 3C commands.
import { rkeys, useRecordApi, useRecordCommand } from "../record/hooks";
import type { Clearance, ExitKind, GuardianRelationship, QualificationCompletion, Scholarship, ScholarshipEligibility, StudentExit } from "./types";

export { C3ApiProvider } from "./hooks";
export { createMockC3Api } from "./mock";

const EXIT_KINDS: { value: ExitKind; label: string }[] = [
  { value: "withdrawal", label: "Withdrawal" },
  { value: "transfer", label: "Transfer out" },
  { value: "discontinuation", label: "Discontinuation" },
  { value: "termination", label: "Termination" },
  { value: "administrative_closure", label: "Administrative closure" },
  { value: "deceased", label: "Deceased record closure" },
];

/** Next commands of a completion by status (server rules stay authoritative). */
export function completionActions(c: QualificationCompletion): CompletionCommand[] {
  switch (c.status) {
    case "eligibility_evaluation":
      return ["eligibility-evaluation", "defer", "cancel"];
    case "degree_audit":
      return ["degree-audit", "defer", "cancel"];
    case "institutional_clearance":
      return ["defer", "cancel"];
    case "approval_pending":
      return ["decision"];
    case "approved":
      return ["award"];
    case "awarded":
      return ["finalise"];
    case "record_finalized":
      return ["complete-exit"];
    case "deferred":
      return ["resume", "cancel"];
    default:
      return [];
  }
}

export function relationshipActions(r: GuardianRelationship): RelationshipCommand[] {
  switch (r.status) {
    case "proposed":
      return ["submit-verification", "terminate"];
    case "pending_verification":
      return ["verification-decision", "terminate"];
    case "verified":
      return ["activate", "terminate"];
    case "active":
      return ["suspend", "terminate"];
    case "suspended":
      return ["reinstate", "terminate"];
    default:
      return [];
  }
}

export function scholarshipActions(s: Scholarship): ScholarshipCommand[] {
  switch (s.status) {
    case "draft":
      return ["publish"];
    case "published":
      return ["activate", "archive"];
    case "active":
      return ["suspend", "expire"];
    case "suspended":
      return ["activate", "archive"];
    case "expired":
      return ["archive"];
    default:
      return [];
  }
}

export function eligibilityActions(e: ScholarshipEligibility): EligibilityCommand[] {
  switch (e.status) {
    case "eligible":
      return e.recommendationPending ? ["recommendation-decision"] : ["recommend", "expire"];
    case "conditionally_eligible":
      return ["resolve-condition", "expire"];
    case "recommendation_generated":
      return ["hand-off", "withdraw-recommendation"];
    case "active":
      return ["renew", "reassess", "expire"];
    case "expired":
    case "not_eligible":
      return ["archive"];
    default:
      return [];
  }
}

const label = (s: string) => statusLabel(s.replace(/-/g, "_"));

/** The institution of a student's current (else latest) context — scopes guardian/scholarship pickers. */
function useStudentInstitution(studentId: string): string | null {
  const api = useRecordApi();
  const q = useQuery({ queryKey: rkeys.scoped("student", studentId), queryFn: () => api.getStudent(studentId), enabled: !!studentId });
  return q.data?.currentContext?.institutionId ?? q.data?.contexts[0]?.institutionId ?? null;
}

const fullName = (g: { givenName: string; familyName?: string | null }) => [g.givenName, g.familyName].filter(Boolean).join(" ");

function ClearanceList({ items, onResolve }: { items: Clearance[]; onResolve?: (c: Clearance) => void }) {
  if (items.length === 0) return null;
  return (
    <ul className="sb-list" data-clearances>
      {items.map((c) => (
        <li key={c.id} data-clearance={c.item}>
          {statusLabel(c.item)} <StatusBadge status={c.status} />
          {c.note ? <span className="sb-muted"> · {c.note}</span> : null}
          {onResolve && c.item !== "finance" && c.status !== "cleared" && c.status !== "waived" ? (
            <Button size="sm" variant="ghost" onClick={() => onResolve(c)} data-action={`resolve-${c.item}`}>
              Resolve
            </Button>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

// ---- Admin → Students → Completion & exits --------------------------------------------------

export function ExitsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useC3Api();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const recordApi = useRecordApi();
  const q = useQuery({ queryKey: k3("exits", institution?.id ?? "-"), queryFn: () => api.listExits(institution!.id), enabled: !!institution });
  const students = useQuery({ queryKey: rkeys.scoped(institution?.id ?? "-", "directory", ""), queryFn: () => recordApi.directory(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const studentName = (id: string) => students.data?.find((s) => s.id === id)?.displayName ?? "Student";
  const resolve = (e: StudentExit, c: Clearance) =>
    ask({
      title: `Resolve ${statusLabel(c.item)} clearance`,
      name: "exit-clearance",
      submitLabel: "Resolve",
      fields: [
        { name: "status", label: "Outcome", type: "select", required: true, initial: "cleared", options: [{ value: "cleared", label: "Cleared" }, { value: "waived", label: "Waived" }, { value: "blocked", label: "Blocked" }] },
        { name: "note", label: "Note", wide: true },
      ],
      onSubmit: (v) => submit(() => api.resolveExitClearance(e.id, e.version, c.id, { status: v.status, note: v.note?.trim() || undefined }), "Clearance recorded"),
    });
  const actions = (e: StudentExit): RowAction[] => {
    const a: RowAction[] = [];
    if (e.status === "approved") a.push({ id: "exit-complete", label: "Complete", variant: "primary", onClick: () => void run(() => api.exitCommand(e.id, e.version, "complete"), "Exit completed") });
    if (e.status === "approval_pending") {
      a.push({ id: "exit-approve", label: "Approve", onClick: () => void run(() => api.exitCommand(e.id, e.version, "decision", { decision: "approve" }), "Exit approved") });
      a.push({ id: "exit-reject", label: "Reject", onClick: () => ask({ title: "Reject exit", name: "exit-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.exitCommand(e.id, e.version, "decision", { decision: "reject", reason: v.reason!.trim() }), "Exit rejected") }) });
    }
    if (!["completed", "cancelled", "rejected"].includes(e.status))
      a.push({ id: "exit-cancel", label: "Cancel", onClick: () => ask({ title: "Cancel exit", name: "exit-cancel", submitLabel: "Cancel exit", fields: [REASON], onSubmit: (v) => submit(() => api.exitCommand(e.id, e.version, "cancel", { reason: v.reason!.trim() }), "Exit cancelled") }) });
    return a;
  };
  return (
    <>
      <BentoTile span={12} title="Student exits">
        <DataTable
          caption="Student exits"
          rows={q.data ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState title="No exits">Exits are initiated from a student's 360 view.</EmptyState>}
          columns={[
            { key: "student", header: "Student", render: (e) => <Link to={student360Path(e.studentId, "completion")}>{studentName(e.studentId)}</Link> },
            { key: "kind", header: "Kind", render: (e) => <span data-exit={e.kind}>{statusLabel(e.kind)}</span> },
            { key: "reason", header: "Reason", render: (e) => e.reason, muted: true },
            { key: "clearance", header: "Clearance", render: (e) => <ClearanceList items={e.clearances} onResolve={e.status === "clearance_pending" ? (c) => resolve(e, c) : undefined} /> },
            { key: "effective", header: "Effective", render: (e) => fmtDate(e.effectiveOn) },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
            { key: "actions", header: "", render: (e) => <Actions disabled={busy} actions={actions(e)} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ---- Admin → Students → Guardians -----------------------------------------------------------

export function GuardiansAdminTab({ institution }: { institution: Institution | null }) {
  const api = useC3Api();
  const { submit, dialog } = useRecordCommand();
  const [adding, setAdding] = useState(false);
  const q = useQuery({ queryKey: k3("guardians", institution?.id ?? "-"), queryFn: () => api.listGuardians(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  return (
    <>
      <BentoTile
        span={12}
        title="Guardians"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-guardian>
            Register guardian
          </Button>
        }
      >
        <DataTable
          caption="Guardians"
          rows={q.data ?? []}
          rowKey={(g) => g.id}
          empty={<EmptyState title="No guardians yet">Register a guardian, then link them from the student's 360 view.</EmptyState>}
          columns={[
            { key: "name", header: "Name", render: (g) => <span data-guardian={g.givenName}>{[g.givenName, g.familyName].filter(Boolean).join(" ")}</span> },
            { key: "phone", header: "Phone", render: (g) => g.phone ?? "—", muted: true },
            { key: "email", header: "E-mail", render: (g) => g.email ?? "—", muted: true },
          ]}
        />
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="Register guardian">
        <h2>Register guardian</h2>
        <QuickForm
          name="new-guardian"
          submitLabel="Register"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "givenName", label: "Given name", required: true },
            { name: "familyName", label: "Family name" },
            { name: "phone", label: "Phone", placeholder: "+91…" },
            { name: "email", label: "E-mail" },
          ]}
          onSubmit={async (v) => {
            await submit(() => api.createGuardian(institution.id, { givenName: v.givenName!.trim(), familyName: v.familyName?.trim() || undefined, phone: v.phone?.trim() || undefined, email: v.email?.trim() || undefined }), "Guardian registered");
            setAdding(false);
          }}
        />
      </Modal>
      {dialog}
    </>
  );
}

// ---- Admin → Students → Scholarships --------------------------------------------------------

export function ScholarshipsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useC3Api();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const [adding, setAdding] = useState(false);
  const q = useQuery({ queryKey: k3("scholarships", institution?.id ?? "-"), queryFn: () => api.listScholarships(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const act = (s: Scholarship, cmd: ScholarshipCommand) => {
    if (cmd === "suspend")
      return ask({ title: `Suspend ${s.name}`, name: "scholarship-suspend", submitLabel: "Suspend", fields: [REASON], onSubmit: (v) => submit(() => api.scholarshipCommand(s.id, s.version, cmd, v.reason!.trim()), "Scholarship suspended") });
    void run(() => api.scholarshipCommand(s.id, s.version, cmd), `${s.name}: ${label(cmd).toLowerCase()} done`);
  };
  const addPolicy = (s: Scholarship) =>
    ask({
      title: `Eligibility policy for ${s.name}`,
      name: "scholarship-policy",
      submitLabel: "Submit policy",
      fields: [
        { name: "minAcademicScore", label: "Minimum academic score", type: "number" },
        { name: "maxFamilyIncome", label: "Maximum family income (INR)", type: "number" },
        { name: "conditionalEvidence", label: "Missing evidence gives a conditional outcome", type: "checkbox" },
      ],
      onSubmit: (v) =>
        submit(async () => {
          const criteria = {
            minAcademicScore: v.minAcademicScore ? Number(v.minAcademicScore) : undefined,
            maxFamilyIncome: v.maxFamilyIncome ? Number(v.maxFamilyIncome) : undefined,
            conditionalEvidence: v.conditionalEvidence === "true",
          };
          const p = await api.createPolicy(s.id, { criteria });
          return api.policyCommand(p.id, p.version, "submit");
        }, (p) => `Policy version ${p.versionNo}: ${statusLabel(p.status).toLowerCase()}`),
    });
  return (
    <>
      <BentoTile
        span={12}
        title="Scholarships"
        action={
          <div className="sb-row-actions">
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => api.recalculate(institution.id, { reason: "manual run" }), (r) => `Recalculated ${r.evaluated} eligibilities (${r.changed} changed)`)} data-action="scholarship-recalculate">
              Recalculate
            </Button>
            <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-scholarship>
              New scholarship
            </Button>
          </div>
        }
      >
        <DataTable
          caption="Scholarships"
          rows={q.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState title="No scholarships yet">Define a scholarship, give it an eligibility policy and make it available.</EmptyState>}
          columns={[
            { key: "code", header: "Code", render: (s) => s.code },
            { key: "name", header: "Name", render: (s) => <span data-scholarship={s.code}>{s.name}</span> },
            { key: "policy", header: "Policy", render: (s) => (s.effectivePolicy ? `v${s.effectivePolicy.versionNo}` : "—"), muted: true },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "actions",
              header: "",
              render: (s) => (
                <Actions
                  disabled={busy}
                  actions={[
                    ...scholarshipActions(s).map((a) => ({ id: `scholarship-${a}`, label: label(a), onClick: () => act(s, a) })),
                    ...(["draft", "published", "active", "suspended"].includes(s.status) ? [{ id: "scholarship-policy", label: "New policy", onClick: () => addPolicy(s) }] : []),
                  ]}
                />
              ),
            },
          ]}
        />
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="New scholarship">
        <h2>New scholarship</h2>
        <QuickForm
          name="new-scholarship"
          submitLabel="Create draft"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "MERIT-2026" },
            { name: "name", label: "Name", required: true },
            { name: "sponsorType", label: "Sponsor", type: "select", initial: "institutional", options: ["institutional", "government", "private", "external"].map((x) => ({ value: x, label: statusLabel(x) })) },
            { name: "benefitType", label: "Benefit", type: "select", initial: "fee_percentage", options: ["fixed_amount", "fee_percentage", "fee_waiver", "stipend"].map((x) => ({ value: x, label: statusLabel(x) })) },
            { name: "benefitValue", label: "Benefit value", type: "number" },
            { name: "validFrom", label: "Valid from", type: "date", required: true },
            { name: "renewable", label: "Renewable", type: "checkbox" },
          ]}
          onSubmit={async (v) => {
            await submit(
              () =>
                api.createScholarship(institution.id, {
                  code: v.code!.trim().toUpperCase(),
                  name: v.name!.trim(),
                  sponsorType: v.sponsorType,
                  benefitType: v.benefitType,
                  benefitValue: v.benefitValue ? Number(v.benefitValue) : undefined,
                  validFrom: v.validFrom,
                  renewable: v.renewable === "true",
                }),
              "Scholarship drafted",
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

// ---- Student 360 → Guardians ----------------------------------------------------------------

export function Student360Guardians({ studentId }: { studentId: string }) {
  const api = useC3Api();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const rels = useQuery({ queryKey: k3("relationships", studentId), queryFn: () => api.listRelationships(studentId) });
  const contacts = useQuery({ queryKey: k3("contacts", studentId), queryFn: () => api.listEmergencyContacts(studentId) });
  const pickups = useQuery({ queryKey: k3("pickups", studentId), queryFn: () => api.listPickups(studentId) });
  const institutionId = useStudentInstitution(studentId);
  const guardians = useQuery({ queryKey: k3("guardians", institutionId ?? "-"), queryFn: () => api.listGuardians(institutionId!), enabled: !!institutionId });
  const act = (r: GuardianRelationship, cmd: RelationshipCommand) => {
    if (cmd === "submit-verification")
      return ask({
        title: "Submit for verification",
        name: "guardian-verify",
        submitLabel: "Submit",
        fields: [
          { name: "method", label: "Method", type: "select", initial: "document", options: ["document", "in_person", "court_order"].map((x) => ({ value: x, label: statusLabel(x) })) },
          { name: "evidenceRef", label: "Evidence reference", required: true, wide: true },
        ],
        onSubmit: (v) => submit(() => api.relationshipCommand(r.id, r.version, cmd, { method: v.method, evidenceRef: v.evidenceRef!.trim() }), (x) => `Relationship ${statusLabel(x.status).toLowerCase()}`),
      });
    if (cmd === "suspend" || cmd === "terminate" || cmd === "reinstate")
      return ask({ title: `${label(cmd)} relationship`, name: `guardian-${cmd}`, submitLabel: label(cmd), fields: [cmd === "reinstate" ? { ...REASON, required: r.hasLegalCustody } : REASON], onSubmit: (v) => submit(() => api.relationshipCommand(r.id, r.version, cmd, v.reason?.trim() ? { reason: v.reason.trim() } : {}), `Relationship ${label(cmd).toLowerCase()} done`) });
    if (cmd === "verification-decision") return void run(() => api.relationshipCommand(r.id, r.version, cmd, { decision: "approve" }), "Verification approved");
    void run(() => api.relationshipCommand(r.id, r.version, cmd), `Relationship ${label(cmd).toLowerCase()} done`);
  };
  const link = () =>
    ask({
      title: "Link guardian",
      name: "guardian-link",
      submitLabel: "Propose",
      fields: [
        {
          name: "guardianId",
          label: "Guardian",
          type: "select",
          required: true,
          options: [{ value: "", label: "Choose a registered guardian" }, ...(guardians.data ?? []).map((g) => ({ value: g.id, label: `${fullName(g)}${g.phone ? ` · ${g.phone}` : ""}` }))],
          wide: true,
        },
        { name: "relationshipType", label: "Relationship", type: "select", initial: "mother", options: ["mother", "father", "legal_guardian", "grandparent", "sibling", "relative", "sponsor", "other"].map((x) => ({ value: x, label: statusLabel(x) })) },
        { name: "isPrimary", label: "Primary guardian", type: "checkbox" },
      ],
      onSubmit: (v) => submit(() => api.proposeRelationship(studentId, { guardianId: v.guardianId!.trim(), relationshipType: v.relationshipType, isPrimary: v.isPrimary === "true" }), "Relationship proposed"),
    });
  return (
    <>
      <BentoTile span={12} title="Guardian relationships" action={<Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={link} data-new-relationship>Link guardian</Button>}>
        <DataTable
          caption="Guardian relationships"
          rows={rels.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState title="No guardians linked">Link a registered guardian to this student.</EmptyState>}
          columns={[
            { key: "name", header: "Guardian", render: (r) => {
              const g = r.guardian ?? guardians.data?.find((x) => x.id === r.guardianId);
              return <span data-relationship={r.relationshipType}>{g ? fullName(g) : "Guardian"}</span>;
            } },
            { key: "type", header: "Relationship", render: (r) => `${statusLabel(r.relationshipType)}${r.isPrimary ? " · primary" : ""}${r.hasLegalCustody ? " · custody" : ""}`, muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions disabled={busy} actions={relationshipActions(r).map((a) => ({ id: `relationship-${a}`, label: a === "verification-decision" ? "Approve verification" : label(a), onClick: () => act(r, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Emergency contacts"
        action={
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              ask({
                title: "Add emergency contact",
                name: "emergency-contact",
                submitLabel: "Add",
                fields: [
                  { name: "name", label: "Name", required: true },
                  { name: "relation", label: "Relation", required: true },
                  { name: "phone", label: "Phone", required: true },
                  { name: "priority", label: "Priority (1 = first)", type: "number", required: true, initial: "1" },
                ],
                onSubmit: (v) => submit(() => api.addEmergencyContact(studentId, { name: v.name!.trim(), relation: v.relation!.trim(), phone: v.phone!.trim(), priority: Number(v.priority) }), "Emergency contact added"),
              })
            }
            data-new-contact
          >
            Add
          </Button>
        }
      >
        <DataTable
          caption="Emergency contacts"
          rows={(contacts.data ?? []).filter((c) => c.status === "active")}
          rowKey={(c) => c.id}
          empty={<EmptyState title="No emergency contacts" />}
          columns={[
            { key: "p", header: "#", render: (c) => c.priority },
            { key: "name", header: "Name", render: (c) => `${c.name} (${c.relation})` },
            { key: "phone", header: "Phone", render: (c) => c.phone, muted: true },
            { key: "actions", header: "", render: (c) => <Actions disabled={busy} actions={[{ id: "contact-remove", label: "Remove", onClick: () => void run(() => api.removeEmergencyContact(c.id, c.version), "Contact removed") }]} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Pickup authorisations"
        action={
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              ask({
                title: "Authorise pickup",
                name: "pickup",
                submitLabel: "Authorise",
                fields: [
                  { name: "personName", label: "Person", required: true },
                  { name: "idDocumentRef", label: "Identity document", required: true },
                  { name: "validUntil", label: "Valid until", type: "date" },
                ],
                onSubmit: (v) => submit(() => api.grantPickup(studentId, { personName: v.personName!.trim(), idDocumentRef: v.idDocumentRef!.trim(), validUntil: v.validUntil || undefined }), "Pickup authorised"),
              })
            }
            data-new-pickup
          >
            Add
          </Button>
        }
      >
        <DataTable
          caption="Pickup authorisations"
          rows={pickups.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState title="No pickup authorisations" />}
          columns={[
            { key: "name", header: "Person", render: (p) => p.personName },
            { key: "until", header: "Valid until", render: (p) => fmtDate(p.validUntil), muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "actions", header: "", render: (p) => <Actions disabled={busy} actions={p.status === "active" ? [{ id: "pickup-revoke", label: "Revoke", onClick: () => void run(() => api.revokePickup(p.id, p.version), "Pickup revoked") }] : []} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ---- Student 360 → Scholarships -------------------------------------------------------------

export function Student360Scholarships({ studentId }: { studentId: string }) {
  const api = useC3Api();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const q = useQuery({ queryKey: k3("eligibilities", studentId), queryFn: () => api.listEligibilities(studentId) });
  const institutionId = useStudentInstitution(studentId);
  const scholarships = useQuery({ queryKey: k3("scholarships", institutionId ?? "-"), queryFn: () => api.listScholarships(institutionId!), enabled: !!institutionId });
  const schName = (id: string) => {
    const s = scholarships.data?.find((x) => x.id === id);
    return s ? `${s.name} (${s.code})` : "Scholarship";
  };
  const act = (e: ScholarshipEligibility, cmd: EligibilityCommand) => {
    if (cmd === "expire" || cmd === "withdraw-recommendation")
      return ask({ title: label(cmd), name: `eligibility-${cmd}`, submitLabel: label(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.eligibilityCommand(e.id, e.version, cmd, { reason: v.reason!.trim() }), `${label(cmd)} done`) });
    const body = cmd === "resolve-condition" ? { outcome: "fulfilled" } : cmd === "recommendation-decision" ? { decision: "approve" } : {};
    void run(() => api.eligibilityCommand(e.id, e.version, cmd, body), (x) => `Eligibility ${statusLabel(x.status).toLowerCase()}`);
  };
  return (
    <>
      <BentoTile
        span={12}
        title="Scholarship eligibility"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            onClick={() =>
              ask({
                title: "Determine eligibility",
                name: "determine-eligibility",
                submitLabel: "Determine",
                fields: [
                  {
                    name: "scholarshipId",
                    label: "Scholarship",
                    type: "select",
                    required: true,
                    options: [
                      { value: "", label: "Choose an active scholarship" },
                      ...(scholarships.data ?? []).filter((s) => s.status === "active").map((s) => ({ value: s.id, label: `${s.name} (${s.code})` })),
                    ],
                    wide: true,
                  },
                  { name: "academicScore", label: "Academic score", type: "number" },
                  { name: "familyIncome", label: "Family income (INR)", type: "number" },
                ],
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.determine(studentId, {
                        scholarshipId: v.scholarshipId!.trim(),
                        facts: { academicScore: v.academicScore ? Number(v.academicScore) : undefined, familyIncome: v.familyIncome ? Number(v.familyIncome) : undefined },
                      }),
                    (x) => `Determination: ${statusLabel(x.status).toLowerCase()}`,
                  ),
              })
            }
            data-new-eligibility
          >
            Determine
          </Button>
        }
      >
        <DataTable
          caption="Scholarship eligibility"
          rows={q.data ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState title="No scholarship determinations" />}
          columns={[
            { key: "sch", header: "Scholarship", render: (e) => <span data-eligibility={e.status}>{schName(e.scholarshipId)}</span> },
            { key: "cond", header: "Conditions", render: (e) => (e.conditions.length ? e.conditions.join("; ") : "—"), muted: true },
            { key: "until", header: "Valid until", render: (e) => fmtDate(e.validUntil) },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
            { key: "actions", header: "", render: (e) => <Actions disabled={busy} actions={eligibilityActions(e).map((a) => ({ id: `eligibility-${a}`, label: a === "recommendation-decision" ? "Approve recommendation" : label(a), onClick: () => act(e, a) }))} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ---- Student 360 → Completion & exit --------------------------------------------------------

export function Student360Completion({ studentId }: { studentId: string }) {
  const api = useC3Api();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const completions = useQuery({ queryKey: k3("completions", studentId), queryFn: () => api.listCompletions(studentId) });
  const exits = useQuery({ queryKey: k3("student-exits", studentId), queryFn: () => api.listStudentExits(studentId) });
  const conferred = useQuery({ queryKey: k3("conferred", studentId), queryFn: () => api.listConferred(studentId) });
  const alumni = useQuery({ queryKey: k3("alumni", studentId), queryFn: () => api.listAlumni(studentId) });
  const act = (c: QualificationCompletion, cmd: CompletionCommand) => {
    if (cmd === "defer" || cmd === "cancel")
      return ask({ title: `${label(cmd)} completion`, name: `completion-${cmd}`, submitLabel: label(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.completionCommand(c.id, c.version, cmd, { reason: v.reason!.trim() }), `Completion ${label(cmd).toLowerCase()} done`) });
    if (cmd === "eligibility-evaluation")
      return ask({
        title: "Evaluate eligibility",
        name: "completion-eligibility",
        submitLabel: "Evaluate",
        fields: [{ name: "creditsEarned", label: "Credits earned", type: "number", required: true }],
        onSubmit: (v) => submit(() => api.completionCommand(c.id, c.version, cmd, { creditsEarned: Number(v.creditsEarned) }), (x) => (x.status === "degree_audit" ? "Eligible — degree audit next" : "Not yet eligible")),
      });
    if (cmd === "resume") return ask({ title: "Resume completion", name: "completion-resume", submitLabel: "Resume", fields: [{ name: "resolutionNote", label: "Remediation", required: true, wide: true }], onSubmit: (v) => submit(() => api.completionCommand(c.id, c.version, cmd, { resolutionNote: v.resolutionNote!.trim() }), "Completion resumed") });
    const body = cmd === "decision" ? { decision: "approve" } : {};
    void run(() => api.completionCommand(c.id, c.version, cmd, body), (x) => `Completion ${statusLabel(x.status).toLowerCase()}`);
  };
  const openExit = (exits.data ?? []).find((e) => !["completed", "cancelled", "rejected"].includes(e.status));
  const concluded = (exits.data ?? []).some((e) => e.status === "completed");
  // One completion at a time, and none once the student has exited.
  const openCompletion = (completions.data ?? []).some((c) => !["exit_completed", "cancelled"].includes(c.status));
  const canStart = !openCompletion && !openExit && !concluded;
  return (
    <>
      <BentoTile
        span={12}
        title="Qualification completion"
        action={
          canStart ? (
          <div className="sb-row-actions">
            <Button size="sm" disabled={busy} onClick={() => void run(() => api.createCompletion(studentId, { outcome: "degree" }), "Completion initiated")} data-action="completion-start">
              Start completion
            </Button>
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => api.createCompletion(studentId, { outcome: "certificate", pathway: "meme_exit" }), "MEME exit initiated")} data-action="completion-meme">
              MEME exit
            </Button>
          </div>
          ) : null
        }
      >
        <DataTable
          caption="Qualification completions"
          rows={completions.data ?? []}
          rowKey={(c) => c.id}
          empty={<EmptyState title="No completion in progress" />}
          columns={[
            { key: "outcome", header: "Outcome", render: (c) => <span data-completion={c.status}>{statusLabel(c.outcome)}{c.pathway === "meme_exit" ? " · MEME exit" : ""}</span> },
            {
              key: "clearance",
              header: "Clearance",
              render: (c) => (
                <ClearanceList
                  items={c.clearances}
                  onResolve={c.status === "institutional_clearance" ? (x) => void run(() => api.resolveCompletionClearance(c.id, c.version, x.id, { status: "cleared" }), "Clearance recorded") : undefined}
                />
              ),
            },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            { key: "actions", header: "", render: (c) => <Actions disabled={busy} actions={completionActions(c).map((a) => ({ id: `completion-${a}`, label: a === "decision" ? "Approve" : label(a), onClick: () => act(c, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Exit"
        action={
          !openExit && !concluded ? (
            <Button
              size="sm"
              variant="ghost"
              onClick={() =>
                ask({
                  title: "Initiate exit",
                  name: "new-exit",
                  submitLabel: "Initiate",
                  fields: [
                    { name: "kind", label: "Kind", type: "select", initial: "withdrawal", options: EXIT_KINDS },
                    REASON,
                    { name: "destinationInstitution", label: "Destination (transfer)", wide: true },
                    { name: "evidenceRef", label: "Evidence reference (termination / deceased)", wide: true },
                  ],
                  onSubmit: (v) =>
                    submit(
                      () => api.createExit(studentId, { kind: v.kind as ExitKind, reason: v.reason!.trim(), destinationInstitution: v.destinationInstitution?.trim() || undefined, evidenceRef: v.evidenceRef?.trim() || undefined }),
                      "Exit initiated — complete the clearance in Admin → Students → Completion & exits",
                    ),
                })
              }
              data-new-exit
            >
              Initiate exit
            </Button>
          ) : null
        }
      >
        <DataTable
          caption="Exits"
          rows={exits.data ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState title="No exit" />}
          columns={[
            { key: "kind", header: "Kind", render: (e) => statusLabel(e.kind) },
            { key: "effective", header: "Effective", render: (e) => fmtDate(e.effectiveOn), muted: true },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Conferred qualifications">
        <DataTable
          caption="Conferred qualifications"
          rows={conferred.data ?? []}
          rowKey={(q) => q.id}
          empty={<EmptyState title="None conferred" />}
          columns={[
            { key: "title", header: "Title", render: (q) => q.title },
            { key: "on", header: "Conferred", render: (q) => fmtDate(q.conferredOn), muted: true },
            { key: "status", header: "Status", render: (q) => <StatusBadge status={q.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Alumni transition"
        action={
          concluded && (alumni.data ?? []).length === 0 ? (
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => api.initiateAlumni(studentId), "Alumni transition initiated")} data-action="alumni-initiate">
              Initiate
            </Button>
          ) : null
        }
      >
        <DataTable
          caption="Alumni transitions"
          rows={alumni.data ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState title="No alumni transition">Available once the student exit is completed.</EmptyState>}
          columns={[
            { key: "basis", header: "Basis", render: (a) => (a.basis ? statusLabel(a.basis) : "—"), muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
            {
              key: "actions",
              header: "",
              render: (a) => {
                const next = ({ pending_verification: "validate-eligibility", approved: "execute", activated: "complete", completed: "archive" } as const)[a.status as "pending_verification"];
                if (a.status === "verified")
                  return (
                    <Actions
                      disabled={busy}
                      actions={[
                        {
                          id: "alumni-validate-policy",
                          label: "Validate policy",
                          onClick: () =>
                            ask({
                              title: "Validate transition policy",
                              name: "alumni-policy",
                              submitLabel: "Validate",
                              fields: [{ name: "graduationYear", label: "Graduation year", type: "number", required: true, initial: String(new Date().getFullYear()) }],
                              onSubmit: (v) => submit(() => api.alumniCommand(a.id, a.version, "validate-policy", { graduationYear: Number(v.graduationYear) }), (x) => `Alumni transition ${statusLabel(x.status).toLowerCase()}`),
                            }),
                        },
                      ]}
                    />
                  );
                return next ? <Actions disabled={busy} actions={[{ id: `alumni-${next}`, label: label(next), onClick: () => void run(() => api.alumniCommand(a.id, a.version, next), (x) => `Alumni transition ${statusLabel(x.status).toLowerCase()}`) }]} /> : null;
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
