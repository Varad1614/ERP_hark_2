/**
 * Student 360 tabs owned by 3A — overview (persona, status, profile, suspension / restoration,
 * archival, identity correction, status history), enrolment (current context, enrolment,
 * assignment, transfers, record history) and documents (record documents + requests).
 * Faculty see everything read-only; administrators get the actions.
 */
import { FilePlus2 } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { useSession } from "../../../lib/auth";
import { Actions, fmtDate, REASON, useAsk } from "../../institution/forms";
import { keys as instKeys, StatusBadge, useInstitutionApi, useInstitutions } from "../../institution/hooks";
import { shortId, type Student } from "./api";
import { rkeys, useRecordApi, useRecordCommand } from "./hooks";

export const REQUEST_TYPES = [
  "bonafide_certificate",
  "transcript_copy",
  "migration_certificate",
  "character_certificate",
  "conduct_certificate",
  "enrolment_letter",
  "id_card_reissue",
] as const;

export const humanise = (s: string) => s.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

function useStudent(studentId: string) {
  const api = useRecordApi();
  return useQuery({ queryKey: rkeys.scoped("student", studentId), queryFn: () => api.getStudent(studentId), enabled: !!studentId });
}

function Facts({ rows }: { rows: Array<[string, ReactNode]> }) {
  return (
    <dl className="sb-facts">
      {rows.map(([k, v]) => (
        <div key={k}>
          <dt>{k}</dt>
          <dd>{v ?? "—"}</dd>
        </div>
      ))}
    </dl>
  );
}

function Loading({ q, children }: { q: { isLoading: boolean; error: unknown; data?: Student }; children: (s: Student) => ReactNode }) {
  if (q.isLoading) return <BentoTile span={12}><p>Loading student…</p></BentoTile>;
  if (!q.data) return <BentoTile span={12}><EmptyState title="Student not found">The record does not exist or belongs to another tenant.</EmptyState></BentoTile>;
  return <>{children(q.data)}</>;
}

export function Student360Overview({ studentId }: { studentId: string }) {
  const api = useRecordApi();
  const { role } = useSession();
  const admin = role === "administrator";
  const q = useStudent(studentId);
  const history = useQuery({ queryKey: rkeys.scoped("student", studentId, "status-history"), queryFn: () => api.statusHistory(studentId), enabled: !!studentId });
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  void run;
  return (
    <Loading q={q}>
      {(s) => {
        const c = s.currentContext;
        const actions = [];
        if (admin && s.status !== "archived") {
          actions.push({
            id: "student-profile",
            label: "Edit profile",
            onClick: () =>
              ask({
                title: "Edit profile",
                name: "student-profile",
                submitLabel: "Save",
                fields: [
                  { name: "preferredName", label: "Preferred name", initial: s.preferredName ?? "" },
                  { name: "primaryEmail", label: "E-mail", initial: s.primaryEmail ?? "" },
                  { name: "primaryPhone", label: "Phone", initial: s.primaryPhone ?? "" },
                ],
                onSubmit: (v) => submit(() => api.updateProfile(s.id, s.version, { preferredName: v.preferredName, primaryEmail: v.primaryEmail, primaryPhone: v.primaryPhone }), "Profile updated"),
              }),
          });
          actions.push({
            id: "student-identity",
            label: "Correct identity",
            onClick: () =>
              ask({
                title: "Identity correction",
                name: "student-identity",
                submitLabel: "Submit for approval",
                fields: [
                  { name: "givenName", label: "Given name", initial: s.givenName },
                  { name: "familyName", label: "Family name", initial: s.familyName ?? "" },
                  { name: "dateOfBirth", label: "Date of birth", type: "date", initial: s.dateOfBirth ?? "" },
                  { name: "evidence", label: "Supporting evidence", required: true, wide: true },
                ],
                onSubmit: (v) =>
                  submit(async () => {
                    const changes: Record<string, string> = {};
                    if (v.givenName && v.givenName !== s.givenName) changes.givenName = v.givenName;
                    if ((v.familyName ?? "") !== (s.familyName ?? "")) changes.familyName = v.familyName ?? "";
                    if (v.dateOfBirth && v.dateOfBirth !== s.dateOfBirth) changes.dateOfBirth = v.dateOfBirth;
                    const ic = await api.requestCorrection(s.id, changes, v.evidence!);
                    return api.submitCorrection(ic.id, ic.version);
                  }, (r) => (r.status === "completed" ? "Identity corrected" : "Correction sent for approval")),
              }),
          });
          if (c?.status === "active")
            actions.push({
              id: "student-suspend",
              label: "Suspend",
              onClick: () =>
                ask({
                  title: "Suspend student",
                  name: "student-suspend",
                  submitLabel: "Suspend",
                  fields: [
                    { name: "category", label: "Category", type: "select", initial: "administrative", options: ["administrative", "disciplinary", "financial", "academic", "medical", "other"].map((x) => ({ value: x, label: humanise(x) })) },
                    REASON,
                  ],
                  onSubmit: (v) => submit(() => api.suspend(s.id, c.version, v.reason!, v.category!), "Student suspended"),
                }),
            });
          if (c?.status === "suspended")
            actions.push({ id: "student-restore", label: "Restore", variant: "primary" as const, onClick: () => ask({ title: "Restore student", name: "student-restore", submitLabel: "Restore", fields: [REASON], onSubmit: (v) => submit(() => api.restore(s.id, c.version, v.reason!), "Student restored") }) });
          if (!s.contexts.some((x) => ["pending_activation", "active", "suspended"].includes(x.status)) && s.contexts.length > 0)
            actions.push({ id: "student-archive", label: "Archive record", onClick: () => ask({ title: "Archive the student record", name: "student-archive", submitLabel: "Archive", fields: [REASON], onSubmit: (v) => submit(() => api.archive(s.id, s.version, v.reason!), "Record archived") }) });
        }
        return (
          <>
            <BentoTile span={8} title={s.displayName} action={<StatusBadge status={s.status} />}>
              <Facts
                rows={[
                  ["Student number", s.studentNumber],
                  ["Preferred name", s.preferredName],
                  ["Date of birth", fmtDate(s.dateOfBirth)],
                  ["E-mail", s.primaryEmail],
                  ["Phone", s.primaryPhone],
                  ["Self-service login", s.subjectLinked ? "Linked" : "Not linked"],
                  ["Merged into", s.mergedIntoId ? shortId(s.mergedIntoId) : null],
                ]}
              />
              {admin && <Actions disabled={busy} actions={actions} />}
            </BentoTile>
            <BentoTile span={4} title="Current enrolment" data-current-context={c?.status ?? "none"}>
              {c ? (
                <Facts
                  rows={[
                    ["Status", <StatusBadge key="s" status={c.status} />],
                    ["Enrolment no.", c.enrolmentNumber],
                    ["Term", c.currentTermNo ? `Term ${c.currentTermNo}` : null],
                    ["Batch · section", [c.batchCode, c.sectionCode].filter(Boolean).join(" · ") || null],
                  ]}
                />
              ) : (
                <EmptyState title="No enrolment">The student has no institution context.</EmptyState>
              )}
            </BentoTile>
            <BentoTile span={12} title="Status history">
              <DataTable
                caption="Status history"
                rows={[...(history.data ?? [])].reverse()}
                rowKey={(h) => `${h.occurredAt}-${h.scope}-${h.toStatus}`}
                empty={<EmptyState title="No changes yet">Status changes appear here.</EmptyState>}
                columns={[
                  { key: "when", header: "When", render: (h) => new Date(h.occurredAt).toLocaleString("en-IN"), muted: true },
                  { key: "scope", header: "Of", render: (h) => (h.scope === "context" ? "Enrolment" : "Student") },
                  { key: "change", header: "Change", render: (h) => `${humanise(h.fromStatus ?? "—")} → ${humanise(h.toStatus)}` },
                  { key: "reason", header: "Reason", render: (h) => h.reason ?? "—", muted: true },
                ]}
              />
            </BentoTile>
            {prompt}
            {dialog}
          </>
        );
      }}
    </Loading>
  );
}

/** Display names of the institutions and the programmes of `institutionId` (ids → names). */
function useNames(institutionId: string | null | undefined) {
  const instApi = useInstitutionApi();
  const { list } = useInstitutions();
  const programmes = useQuery({ queryKey: instKeys.scoped(institutionId ?? "-", "programmes"), queryFn: () => instApi.listProgrammes(institutionId!), enabled: !!institutionId });
  return {
    institution: (id: string | null | undefined) => list.find((i) => i.id === id)?.name ?? shortId(id),
    programme: (id: string | null | undefined) => {
      const p = programmes.data?.find((x) => x.id === id);
      return p ? `${p.name} (${p.code})` : shortId(id);
    },
  };
}

export function Student360Enrolment({ studentId }: { studentId: string }) {
  const api = useRecordApi();
  const { role } = useSession();
  const admin = role === "administrator";
  const q = useStudent(studentId);
  const history = useQuery({ queryKey: rkeys.scoped("student", studentId, "record-history"), queryFn: () => api.recordHistory(studentId), enabled: !!studentId });
  const transfers = useQuery({ queryKey: rkeys.scoped("student", studentId, "transfers"), queryFn: () => api.listTransfers(studentId), enabled: !!studentId });
  const names = useNames(q.data?.currentContext?.institutionId ?? q.data?.contexts[0]?.institutionId);
  const { submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  return (
    <Loading q={q}>
      {(s) => {
        const c = s.currentContext;
        const actions = [];
        if (admin && c?.status === "pending_activation")
          actions.push({
            id: "student-enrol",
            label: "Complete enrolment",
            variant: "primary" as const,
            onClick: () =>
              ask({
                title: "Complete enrolment",
                name: "student-enrol",
                submitLabel: "Enrol",
                fields: [
                  { name: "enrolmentNumber", label: "Enrolment number (blank = allocate)" },
                  { name: "rollNumber", label: "Roll number" },
                  { name: "batchCode", label: "Batch", initial: c.batchCode ?? "" },
                  { name: "sectionCode", label: "Section", initial: c.sectionCode ?? "" },
                ],
                onSubmit: (v) => submit(() => api.enrol(s.id, c.version, { enrolmentNumber: v.enrolmentNumber || null, rollNumber: v.rollNumber || null, batchCode: v.batchCode || null, sectionCode: v.sectionCode || null }), "Enrolment completed"),
              }),
          });
        if (admin && c?.status === "active") {
          actions.push({
            id: "student-enrolment-update",
            label: "Update enrolment",
            onClick: () =>
              ask({
                title: "Update enrolment",
                name: "student-enrolment-update",
                submitLabel: "Save",
                fields: [
                  { name: "enrolmentNumber", label: "Enrolment number", initial: c.enrolmentNumber ?? "" },
                  { name: "rollNumber", label: "Roll number", initial: c.rollNumber ?? "" },
                  { name: "reason", label: "Reason", wide: true },
                ],
                onSubmit: (v) => submit(() => api.updateEnrolment(s.id, c.version, { enrolmentNumber: v.enrolmentNumber || null, rollNumber: v.rollNumber, reason: v.reason || null }), "Enrolment updated"),
              }),
          });
          actions.push({
            id: "student-assignment",
            label: "Change batch / section",
            onClick: () =>
              ask({
                title: "Academic assignment",
                name: "student-assignment",
                submitLabel: "Save",
                fields: [
                  { name: "batchCode", label: "Batch", initial: c.batchCode ?? "" },
                  { name: "sectionCode", label: "Section", initial: c.sectionCode ?? "" },
                  { name: "reason", label: "Reason", wide: true },
                ],
                onSubmit: (v) => submit(() => api.changeAssignment(s.id, c.version, { batchCode: v.batchCode, sectionCode: v.sectionCode, reason: v.reason || null }), "Assignment changed"),
              }),
          });
          actions.push({
            id: "student-transfer",
            label: "Transfer",
            onClick: () =>
              ask({
                title: "Transfer the student",
                name: "student-transfer",
                submitLabel: "Request + submit",
                fields: [
                  { name: "kind", label: "Kind", type: "select", initial: "intra_institute", options: [{ value: "intra_institute", label: "Within this institution" }, { value: "inter_institute", label: "To another institution" }] },
                  { name: "toInstitutionId", label: "Destination institution id (inter)" },
                  { name: "toProgrammeId", label: "Destination programme id (inter)" },
                  { name: "toSectionCode", label: "Destination section" },
                  REASON,
                ],
                onSubmit: (v) =>
                  submit(async () => {
                    const inter = v.kind === "inter_institute";
                    const t = await api.requestTransfer(s.id, {
                      kind: inter ? "inter_institute" : "intra_institute",
                      toInstitutionId: inter ? v.toInstitutionId || null : null,
                      toProgrammeId: inter ? v.toProgrammeId || null : null,
                      toSectionCode: v.toSectionCode || null,
                      reason: v.reason!,
                    });
                    return api.submitTransfer(t.id, t.version);
                  }, (t) => (t.status === "completed" ? "Transfer completed" : "Transfer sent for approval")),
              }),
          });
        }
        return (
          <>
            <BentoTile span={12} title="Current enrolment" data-context-status={c?.status ?? "none"}>
              {c ? (
                <>
                  <Facts
                    rows={[
                      ["Status", <StatusBadge key="s" status={c.status} />],
                      ["Enrolment number", c.enrolmentNumber],
                      ["Roll number", c.rollNumber],
                      ["Type", humanise(c.enrolmentType)],
                      ["Enrolled on", fmtDate(c.enrolledOn)],
                      ["Term", c.currentTermNo ? `Term ${c.currentTermNo}` : null],
                      ["Batch · section", [c.batchCode, c.sectionCode].filter(Boolean).join(" · ") || null],
                      ["Programme", names.programme(c.programmeId)],
                    ]}
                  />
                  {admin && <Actions disabled={busy} actions={actions} />}
                </>
              ) : (
                <EmptyState title="No enrolment">The student has no institution context.</EmptyState>
              )}
            </BentoTile>
            <BentoTile span={6} title="All enrolments">
              <DataTable
                caption="Contexts"
                rows={s.contexts}
                rowKey={(x) => x.id}
                columns={[
                  { key: "inst", header: "Institution", render: (x) => names.institution(x.institutionId), muted: true },
                  { key: "type", header: "Type", render: (x) => humanise(x.enrolmentType) },
                  { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
                  { key: "closed", header: "Closed", render: (x) => fmtDate(x.closedOn), muted: true },
                ]}
              />
            </BentoTile>
            <BentoTile span={6} title="Transfers">
              <DataTable
                caption="Transfers"
                rows={transfers.data ?? []}
                rowKey={(t) => t.id}
                empty={<EmptyState title="No transfers">Intra- and inter-institute transfers appear here.</EmptyState>}
                columns={[
                  { key: "kind", header: "Kind", render: (t) => humanise(t.kind) },
                  { key: "on", header: "Effective", render: (t) => fmtDate(t.effectiveOn), muted: true },
                  { key: "status", header: "Status", render: (t) => <StatusBadge status={t.status} /> },
                ]}
              />
            </BentoTile>
            <BentoTile span={12} title="Enrolment & assignment history">
              <DataTable
                caption="Record history"
                rows={[...(history.data ?? [])].reverse()}
                rowKey={(h) => `${h.occurredAt}-${h.kind}`}
                empty={<EmptyState title="No changes yet">Enrolment, assignment and profile changes appear here.</EmptyState>}
                columns={[
                  { key: "when", header: "When", render: (h) => new Date(h.occurredAt).toLocaleString("en-IN"), muted: true },
                  { key: "kind", header: "What", render: (h) => humanise(h.kind) },
                  { key: "reason", header: "Reason", render: (h) => h.reason ?? "—", muted: true },
                ]}
              />
            </BentoTile>
            {prompt}
            {dialog}
          </>
        );
      }}
    </Loading>
  );
}

export function Student360Documents({ studentId }: { studentId: string }) {
  const api = useRecordApi();
  const { role } = useSession();
  const admin = role === "administrator";
  const docs = useQuery({ queryKey: rkeys.scoped("student", studentId, "documents"), queryFn: () => api.documents(studentId), enabled: !!studentId });
  const requests = useQuery({ queryKey: rkeys.scoped("student", studentId, "requests"), queryFn: () => api.studentRequests(studentId), enabled: !!studentId });
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  return (
    <>
      <BentoTile
        span={12}
        title="Documents on record"
        action={
          admin ? (
            <Button
              size="sm"
              icon={<FilePlus2 strokeWidth={1.75} />}
              data-add-student-document
              onClick={() =>
                ask({
                  title: "File a document",
                  name: "student-document",
                  submitLabel: "File",
                  fields: [
                    { name: "documentType", label: "Type (snake_case)", required: true, placeholder: "transfer_certificate" },
                    { name: "title", label: "Title", required: true },
                    { name: "fileRef", label: "Storage reference" },
                  ],
                  onSubmit: (v) => submit(() => api.addDocument(studentId, { documentType: v.documentType!.trim(), title: v.title!.trim(), fileRef: v.fileRef || undefined }), "Document filed"),
                })
              }
            >
              File document
            </Button>
          ) : undefined
        }
      >
        <DataTable
          caption="Student documents"
          rows={docs.data ?? []}
          rowKey={(d) => d.id}
          empty={<EmptyState title="No documents">Verified admission documents and issued certificates appear here.</EmptyState>}
          columns={[
            { key: "type", header: "Type", render: (d) => <span data-document={d.documentType}>{humanise(d.documentType)}</span> },
            { key: "title", header: "Title", render: (d) => d.title, muted: true },
            { key: "source", header: "Source", render: (d) => humanise(d.source), muted: true },
            { key: "status", header: "Status", render: (d) => <StatusBadge status={d.status} /> },
            {
              key: "actions",
              header: "",
              render: (d) =>
                admin && d.status === "submitted" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      { id: "student-doc-verify", label: "Verify", onClick: () => void run(() => api.verifyDocument(studentId, d.id, d.version, "verified"), "Document verified") },
                      { id: "student-doc-reject", label: "Reject", onClick: () => ask({ title: "Reject document", name: "student-doc-reject", submitLabel: "Reject", fields: [{ name: "note", label: "Why", required: true, wide: true }], onSubmit: (v) => submit(() => api.verifyDocument(studentId, d.id, d.version, "rejected", v.note), "Document rejected") }) },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Document & certificate requests"
        action={
          admin ? (
            <Button
              size="sm"
              variant="ghost"
              data-raise-request
              onClick={() =>
                ask({
                  title: "Raise a request",
                  name: "student-request",
                  submitLabel: "Raise",
                  fields: [
                    { name: "documentType", label: "Document", type: "select", initial: REQUEST_TYPES[0], options: REQUEST_TYPES.map((t) => ({ value: t, label: humanise(t) })) },
                    { name: "purpose", label: "Purpose", wide: true },
                  ],
                  onSubmit: (v) => submit(() => api.raiseRequest(studentId, { documentType: v.documentType!, purpose: v.purpose || null }), "Request raised"),
                })
              }
            >
              Raise request
            </Button>
          ) : undefined
        }
      >
        <DataTable
          caption="Document requests"
          rows={requests.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState title="No requests">Requests raised by the student or staff appear here.</EmptyState>}
          columns={[
            { key: "type", header: "Document", render: (r) => <span data-request={r.documentType}>{humanise(r.documentType)}</span> },
            { key: "channel", header: "Raised", render: (r) => (r.channel === "self_service" ? "By student" : "By staff"), muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "ref", header: "Reference", render: (r) => r.issuedReference ?? r.statusReason ?? "—", muted: true },
            {
              key: "actions",
              header: "",
              render: (r) =>
                admin && (r.status === "requested" || r.status === "in_progress") ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      ...(r.status === "requested" ? [{ id: "request-start", label: "Start", onClick: () => void run(() => api.requestCommand(r.id, r.version, "start"), "Request in progress") }] : []),
                      { id: "request-issue", label: "Issue", variant: "primary" as const, onClick: () => ask({ title: "Record issue", name: "request-issue", submitLabel: "Issue", fields: [{ name: "issuedReference", label: "Document reference", required: true }], onSubmit: (v) => submit(() => api.issueRequest(r.id, r.version, v.issuedReference!.trim()), "Document issued") }) },
                      { id: "request-reject", label: "Reject", onClick: () => ask({ title: "Reject request", name: "request-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.requestCommand(r.id, r.version, "reject", v.reason), "Request rejected") }) },
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
