/**
 * Admin → Students: the directory and the admissions desk (applications + Admission Records with
 * their next actions, documents, approval decision, clearance, confirmation, student creation,
 * cancellation / withdrawal / revocation / expiry / readmission).
 */
import { FilePlus2, Plus, Search } from "lucide-react";
import { BentoTile, Button, DataTable, Drawer, EmptyState, Modal, SearchField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link } from "react-router";
import { Actions, fmtDate, NoInstitution, QuickForm, REASON, useAsk } from "../../institution/forms";
import { keys as instKeys, StatusBadge, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { student360Path } from "../index";
import { admissionActions, type Admission, type AdmissionCommand, type AdmissionApplication } from "./api";
import { rkeys, useRecordApi, useRecordCommand } from "./hooks";

const ADMISSION_LABEL: Record<string, string> = {
  accept: "Record acceptance",
  documents: "Documents",
  "submit-documents": "Documents submitted",
  "submit-for-approval": "Send for approval",
  decide: "Decide",
  "financial-clearance": "Financial clearance",
  confirm: "Confirm",
  "create-student": "Create student",
  cancel: "Cancel",
  withdraw: "Withdraw",
  revoke: "Revoke",
  expire: "Expire offer",
  readmit: "Readmit",
};

/** Commands that need a reason in the UI (the API enforces it too). */
const NEEDS_REASON: ReadonlySet<string> = new Set(["cancel", "withdraw"]);

export function StudentsDirectoryTab({ institution }: { institution: Institution | null }) {
  const api = useRecordApi();
  const [q, setQ] = useState("");
  const inst = institution?.id ?? "-";
  const list = useQuery({ queryKey: rkeys.scoped(inst, "directory", q), queryFn: () => api.directory(institution!.id, q || undefined), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  return (
    <BentoTile span={12} title="Student directory" action={<div style={{ minWidth: 260 }}><SearchField label="Search students" placeholder="Name or number" value={q} onChange={(e) => setQ(e.target.value)} /></div>}>
      <DataTable
        caption="Students"
        rows={list.data ?? []}
        rowKey={(s) => s.id}
        empty={<EmptyState icon={Search} title={q ? "No matching students" : "No students yet"}>{q ? "Try another name or number." : "Students appear here once an admission is confirmed and the student is created."}</EmptyState>}
        columns={[
          { key: "number", header: "Number", render: (s) => s.studentNumber },
          { key: "name", header: "Name", render: (s) => <Link to={student360Path(s.id)} data-student={s.studentNumber}>{s.displayName}</Link> },
          { key: "enrolment", header: "Enrolment", render: (s) => s.currentContext?.enrolmentNumber ?? "—", muted: true },
          { key: "placement", header: "Batch / section", render: (s) => [s.currentContext?.batchCode, s.currentContext?.sectionCode].filter(Boolean).join(" · ") || "—", muted: true },
          { key: "ctx", header: "Enrolment status", render: (s) => (s.currentContext ? <StatusBadge status={s.currentContext.status} /> : "—") },
          { key: "status", header: "Student", render: (s) => <StatusBadge status={s.status} /> },
        ]}
      />
    </BentoTile>
  );
}

export function AdmissionsTab({ institution }: { institution: Institution | null }) {
  const api = useRecordApi();
  const instApi = useInstitutionApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const [adding, setAdding] = useState(false);
  const [docsFor, setDocsFor] = useState<Admission | null>(null);
  const inst = institution?.id ?? "-";
  const apps = useQuery({ queryKey: rkeys.scoped(inst, "applications"), queryFn: () => api.listApplications(institution!.id), enabled: !!institution });
  const adms = useQuery({ queryKey: rkeys.scoped(inst, "admissions"), queryFn: () => api.listAdmissions(institution!.id), enabled: !!institution });
  const programmes = useQuery({ queryKey: instKeys.scoped(inst, "programmes"), queryFn: () => instApi.listProgrammes(institution!.id), enabled: !!institution });
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const published = (programmes.data ?? []).filter((p) => p.status === "published");
  const progName = (id: string) => programmes.data?.find((p) => p.id === id)?.name ?? "—";

  const appActions = (a: AdmissionApplication) => {
    const out = [];
    if (a.status === "draft") out.push({ id: "application-submit", label: "Submit", onClick: () => void run(() => api.applicationCommand(a.id, a.version, "submit"), "Application submitted") });
    if (a.status === "submitted") out.push({ id: "application-review", label: "Start review", onClick: () => void run(() => api.applicationCommand(a.id, a.version, "review"), "Review started") });
    if (a.status === "submitted" || a.status === "under_review") {
      out.push({
        id: "application-offer",
        label: "Offer admission",
        variant: "primary" as const,
        onClick: () =>
          ask({
            title: `Offer admission to ${a.displayName}`,
            name: "offer",
            submitLabel: "Offer",
            fields: [
              { name: "offerExpiresOn", label: "Offer valid until", type: "date" },
              { name: "requiredDocuments", label: "Required documents (comma-separated)", placeholder: "identity_proof, qualifying_marksheet", wide: true },
            ],
            onSubmit: (v) =>
              submit(
                () =>
                  api.offer(a.id, a.version, {
                    offerExpiresOn: v.offerExpiresOn || undefined,
                    requiredDocuments: v.requiredDocuments ? v.requiredDocuments.split(",").map((x) => x.trim()).filter(Boolean) : undefined,
                  }),
                "Offer made — admission initiated",
              ),
          }),
      });
      out.push({ id: "application-reject", label: "Reject", onClick: () => ask({ title: "Reject application", name: "reject-application", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.applicationCommand(a.id, a.version, "reject", v.reason), "Application rejected") }) });
    }
    if (["draft", "submitted", "under_review"].includes(a.status))
      out.push({ id: "application-withdraw", label: "Withdraw", onClick: () => ask({ title: "Withdraw application", name: "withdraw-application", submitLabel: "Withdraw", fields: [REASON], onSubmit: (v) => submit(() => api.applicationCommand(a.id, a.version, "withdraw", v.reason), "Application withdrawn") }) });
    return out;
  };

  const admAction = (a: Admission, k: ReturnType<typeof admissionActions>[number]) => {
    const label = ADMISSION_LABEL[k] ?? k;
    const onClick = () => {
      if (k === "documents") return setDocsFor(a);
      if (k === "decide")
        return ask({
          title: `Approval decision — ${a.displayName}`,
          name: "admission-decision",
          submitLabel: "Record decision",
          fields: [
            { name: "decision", label: "Decision", type: "select", initial: "approve", options: [{ value: "approve", label: "Approve" }, { value: "reject", label: "Reject" }] },
            { name: "reason", label: "Reason (required to reject)", wide: true },
          ],
          onSubmit: (v) => submit(() => api.decideAdmission(a.id, a.version, v.decision === "reject" ? "reject" : "approve", v.reason || undefined), "Decision recorded"),
        });
      if (k === "revoke")
        return ask({
          title: `Revoke admission — ${a.displayName}`,
          name: "admission-revoke",
          submitLabel: "Revoke",
          fields: [REASON, { name: "evidence", label: "Supporting evidence", required: true, wide: true }],
          onSubmit: (v) => submit(() => api.revokeAdmission(a.id, a.version, v.reason!, v.evidence!), "Admission revoked"),
        });
      if (k === "readmit") return ask({ title: `Readmit ${a.displayName}`, name: "admission-readmit", submitLabel: "Start readmission", fields: [REASON], onSubmit: (v) => submit(() => api.readmit(a.id, v.reason!), "Readmission started — a new admission was initiated") });
      const cmd = k as AdmissionCommand;
      if (NEEDS_REASON.has(cmd)) return ask({ title: `${label} — ${a.displayName}`, name: `admission-${cmd}`, submitLabel: label, fields: [REASON], onSubmit: (v) => submit(() => api.admissionCommand(a.id, a.version, cmd, v.reason), `${label} done`) });
      void run(() => api.admissionCommand(a.id, a.version, cmd), `${label} done`);
    };
    return { id: `admission-${k}`, label, onClick, variant: k === "create-student" || k === "confirm" ? ("primary" as const) : undefined };
  };

  return (
    <>
      <BentoTile
        span={12}
        title="Applications"
        action={
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} disabled={published.length === 0} data-new-application>
            New application
          </Button>
        }
      >
        <DataTable
          caption="Admission applications"
          rows={apps.data ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState title="No applications yet">{published.length === 0 ? "Publish a programme in Academics first." : "Record the first application."}</EmptyState>}
          columns={[
            { key: "name", header: "Applicant", render: (a) => <span data-application={a.displayName}>{a.displayName}</span> },
            { key: "prog", header: "Programme", render: (a) => progName(a.programmeId), muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
            { key: "actions", header: "", render: (a) => <Actions disabled={busy} actions={appActions(a)} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Admission records"
        action={
          <Button size="sm" variant="ghost" onClick={() => void run(() => api.expireOffers(institution.id), (r) => (r.expired.length ? `${r.expired.length} lapsed offer(s) expired` : "No lapsed offers"))} data-expire-offers>
            Expire lapsed offers
          </Button>
        }
      >
        <DataTable
          caption="Admissions"
          rows={adms.data ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState title="No admission records">An offer on an application creates the admission record.</EmptyState>}
          columns={[
            { key: "name", header: "Candidate", render: (a) => (a.studentId ? <Link to={student360Path(a.studentId)}>{a.displayName}</Link> : <span data-admission={a.displayName}>{a.displayName}</span>) },
            { key: "prog", header: "Programme", render: (a) => progName(a.programmeId), muted: true },
            { key: "offer", header: "Offer until", render: (a) => `${fmtDate(a.offerExpiresOn)}${a.offerAcceptedAt ? " · accepted" : ""}`, muted: true },
            { key: "type", header: "Type", render: (a) => a.enrolmentType.replace(/_/g, " "), muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
            { key: "actions", header: "", render: (a) => <Actions disabled={busy} actions={admissionActions(a).map((k) => admAction(a, k))} /> },
          ]}
        />
      </BentoTile>
      <Modal open={adding} onClose={() => setAdding(false)} label="New application">
        <h2>New application</h2>
        <QuickForm
          name="new-application"
          submitLabel="Record application"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "programmeId", label: "Programme", type: "select", initial: published[0]?.id ?? "", options: published.map((p) => ({ value: p.id, label: p.name })), wide: true },
            { name: "givenName", label: "Given name", required: true },
            { name: "familyName", label: "Family name" },
            { name: "dateOfBirth", label: "Date of birth", type: "date" },
            { name: "email", label: "E-mail" },
            { name: "phone", label: "Phone" },
            { name: "subjectRef", label: "Login subject (self-service)", placeholder: "dev|aarav.desai@sutra.dev" },
          ]}
          onSubmit={async (v) => {
            await submit(
              () =>
                api.createApplication(institution.id, {
                  programmeId: v.programmeId!,
                  givenName: v.givenName!.trim(),
                  familyName: v.familyName || null,
                  dateOfBirth: v.dateOfBirth || null,
                  email: v.email || null,
                  phone: v.phone || null,
                  subjectRef: v.subjectRef || null,
                }),
              "Application recorded",
            );
            setAdding(false);
          }}
        />
      </Modal>
      <AdmissionDocumentsDrawer admission={docsFor} onClose={() => setDocsFor(null)} />
      {prompt}
      {dialog}
    </>
  );
}

function AdmissionDocumentsDrawer({ admission, onClose }: { admission: Admission | null; onClose: () => void }) {
  const api = useRecordApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const id = admission?.id ?? "-";
  const docs = useQuery({ queryKey: rkeys.scoped("admission", id, "documents"), queryFn: () => api.admissionDocuments(admission!.id), enabled: !!admission });
  if (!admission) return null;
  const canVerify = admission.status === "documents_submitted";
  return (
    <Drawer open onClose={onClose} title={`Documents — ${admission.displayName}`}>
      <p>Required: {admission.requiredDocuments.map((d) => d.replace(/_/g, " ")).join(", ") || "none"}</p>
      <Button
        size="sm"
        icon={<FilePlus2 strokeWidth={1.75} />}
        onClick={() =>
          ask({
            title: "Hand in a document",
            name: "admission-document",
            submitLabel: "Add",
            fields: [
              { name: "documentType", label: "Type", type: "select", initial: admission.requiredDocuments[0] ?? "identity_proof", options: [...new Set([...admission.requiredDocuments, "photo", "transfer_certificate"])].map((t) => ({ value: t, label: t.replace(/_/g, " ") })) },
              { name: "title", label: "Title", required: true },
              { name: "fileRef", label: "Storage reference" },
            ],
            onSubmit: (v) => submit(() => api.addAdmissionDocument(admission.id, { documentType: v.documentType!, title: v.title!.trim(), fileRef: v.fileRef || undefined }), "Document added"),
          })
        }
        disabled={!["initiated", "documents_submitted"].includes(admission.status)}
        data-add-admission-document
      >
        Add document
      </Button>
      <DataTable
        caption="Admission documents"
        rows={docs.data ?? []}
        rowKey={(d) => d.id}
        empty={<EmptyState title="No documents yet">Add each required document.</EmptyState>}
        columns={[
          { key: "type", header: "Type", render: (d) => d.documentType.replace(/_/g, " ") },
          { key: "title", header: "Title", render: (d) => d.title, muted: true },
          { key: "status", header: "Status", render: (d) => <StatusBadge status={d.status} /> },
          {
            key: "actions",
            header: "",
            render: (d) =>
              canVerify && d.status === "submitted" ? (
                <Actions
                  disabled={busy}
                  actions={[
                    { id: "doc-verify", label: "Verify", variant: "primary", onClick: () => void run(() => api.verifyAdmissionDocument(admission.id, d.id, d.version, "verified"), "Document verified") },
                    { id: "doc-reject", label: "Reject", onClick: () => ask({ title: "Reject document", name: "doc-reject", submitLabel: "Reject", fields: [{ name: "note", label: "Why", required: true, wide: true }], onSubmit: (v) => submit(() => api.verifyAdmissionDocument(admission.id, d.id, d.version, "rejected", v.note), "Document rejected") }) },
                    { id: "doc-waive", label: "Waive", onClick: () => ask({ title: "Waive document", name: "doc-waive", submitLabel: "Waive", fields: [{ name: "note", label: "Why", required: true, wide: true }], onSubmit: (v) => submit(() => api.verifyAdmissionDocument(admission.id, d.id, d.version, "waived", v.note), "Document waived") }) },
                  ]}
                />
              ) : null,
          },
        ]}
      />
      {prompt}
      {dialog}
    </Drawer>
  );
}
