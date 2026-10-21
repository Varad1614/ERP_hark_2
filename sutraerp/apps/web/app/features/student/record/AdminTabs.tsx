/**
 * Admin → Students: the directory (prototype `adm-students`: filter chip rows + "Student records"
 * table with initials, roll no., programme, semester, attendance and fee status) and the
 * admissions desk (applications + Admission Records as list rows with ONE primary action and a
 * kebab; documents in a side panel; offer / decisions in prototype dialogs).
 */
import { ArrowRight, FilePlus2, Search, UserPlus } from "lucide-react";
import {
  Badge,
  BentoTile,
  Button,
  DataTable,
  EmptyState,
  FilterChipRows,
  FilterChips,
  ListRow,
  NameCell,
  SidePanel,
  formatDate,
  humanize,
  useToast,
  type ChipOption,
} from "@sutra/ui";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { Link } from "react-router";
import { keys as instKeys, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { focusFirstInvalid, memberAliases, problemFieldErrors, problemMessage, validateField, validateFields } from "../../../lib/forms";
import { student360Path } from "../index";
import { DialogFrame, NoInstitution, REASON, rowProps, SpecField, useAsk, type FieldSpec, type FormValues, type RowAction } from "../ui";
import { admissionActions, type Admission, type AdmissionApplication, type AdmissionCommand, type AdmissionDocument, type Student } from "./api";
import { rkeys, useRecordApi, useRecordCommand } from "./hooks";
import { AdmissionFunnelTile } from "../funnel/AdmissionFunnelTile";
import { DocumentUploadDialog, useDownload } from "./files";
import { FEE_LABEL, feeStatus, overallAttendance, useAttendanceIndex, useFeeAccounts, type FeeStatus } from "./insights";

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

/** Documents an admission can ask for (offer dialog tags). */
export const ADMISSION_DOCUMENTS = ["identity_proof", "qualifying_marksheet", "transfer_certificate", "photo", "migration_certificate", "category_certificate"] as const;

const FEE_KIND: Record<FeeStatus, "done" | "review" | "attention"> = { paid: "done", partial: "review", due: "review", overdue: "attention" };

export function FeeBadge({ status }: { status: FeeStatus | null }) {
  if (!status) return <span className="is-muted">—</span>;
  return <Badge kind={FEE_KIND[status]}>{FEE_LABEL[status]}</Badge>;
}

export function AttendanceBadge({ percent, threshold }: { percent: number | null | undefined; threshold?: number }) {
  if (percent === null || percent === undefined) return <span className="is-muted">—</span>;
  const min = threshold ?? 75;
  const kind = percent < min ? "attention" : percent < min + 5 ? "review" : "done";
  return <Badge kind={kind}>{`${Math.round(percent)}%`}</Badge>;
}

/** Programme names of an institution (id → name). */
export function useProgrammeNames(institutionId: string | null | undefined) {
  const instApi = useInstitutionApi();
  const programmes = useQuery({ queryKey: instKeys.scoped(institutionId ?? "-", "programmes"), queryFn: () => instApi.listProgrammes(institutionId!), enabled: !!institutionId });
  return { programmes, name: (id: string | null | undefined) => programmes.data?.find((p) => p.id === id)?.name ?? "—" };
}

/** The directory query (shared with the area header count). */
export function useDirectory(institutionId: string | null | undefined, q = "") {
  const api = useRecordApi();
  return useQuery({ queryKey: rkeys.scoped(institutionId ?? "-", "directory", q), queryFn: () => api.directory(institutionId!, q || undefined), enabled: !!institutionId });
}

export function StudentsDirectoryTab({ institution, q = "" }: { institution: Institution | null; q?: string }) {
  const list = useDirectory(institution?.id, q);
  const { name: progName } = useProgrammeNames(institution?.id);
  const fees = useFeeAccounts(institution?.id);
  const attendance = useAttendanceIndex(institution?.id);
  const [prog, setProg] = useState("all");
  const [sem, setSem] = useState("all");
  const [fee, setFee] = useState("all");
  // Roll-number order (prototype: SIT25CE011, SIT22ME014, …), numeric-aware.
  const rows = [...(list.data ?? [])].sort((a, b) => (a.currentContext?.rollNumber ?? a.studentNumber).localeCompare(b.currentContext?.rollNumber ?? b.studentNumber, "en-IN", { numeric: true }));
  const feeOf = (s: Student) => feeStatus(fees.data?.get(s.id));
  const semOf = (s: Student) => (s.currentContext?.currentTermNo ? `t${s.currentContext.currentTermNo}` : "none");
  const chips = (() => {
    const count = (key: (s: Student) => string) => {
      const m = new Map<string, number>();
      for (const s of rows) m.set(key(s), (m.get(key(s)) ?? 0) + 1);
      return m;
    };
    const all = (n: number): ChipOption => ({ id: "all", label: "All", count: n });
    const progs = [...count((s) => s.currentContext?.programmeId ?? "none")].filter(([k]) => k !== "none").map(([k, n]) => ({ id: k, label: progName(k), count: n }));
    const sems = [...count(semOf)].filter(([k]) => k !== "none").sort(([a], [b]) => Number(a.slice(1)) - Number(b.slice(1))).map(([k, n]) => ({ id: k, label: `Sem ${k.slice(1)}`, count: n }));
    const feeCounts = count((s) => feeOf(s) ?? "none");
    const feeOpts = (["paid", "partial", "due", "overdue"] as FeeStatus[]).filter((k) => feeCounts.get(k)).map((k) => ({ id: k, label: FEE_LABEL[k], count: feeCounts.get(k)! }));
    return { all: all(rows.length), progs, sems, feeOpts };
  })();
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const shown = rows.filter(
    (s) => (prog === "all" || s.currentContext?.programmeId === prog) && (sem === "all" || semOf(s) === sem) && (fee === "all" || feeOf(s) === fee),
  );
  return (
    <>
      {rows.length > 0 && (
        <FilterChipRows>
          {chips.progs.length > 0 && <FilterChips label="Programme" options={[chips.all, ...chips.progs]} value={prog} onChange={setProg} data-filter="programme" />}
          {chips.sems.length > 0 && <FilterChips label="Semester" options={[chips.all, ...chips.sems]} value={sem} onChange={setSem} data-filter="semester" />}
          {chips.feeOpts.length > 0 && <FilterChips label="Fee status" options={[chips.all, ...chips.feeOpts]} value={fee} onChange={setFee} data-filter="fee" />}
        </FilterChipRows>
      )}
      <BentoTile span={12} title="Student records" action={<span className="st-count" data-shown={shown.length}>{`${shown.length.toLocaleString("en-IN")} shown`}</span>}>
        <DataTable
          caption="Students"
          rows={shown}
          rowKey={(s) => s.id}
          empty={
            <EmptyState icon={Search} title={q || shown.length < rows.length ? "No matching students" : "No students yet"} compact>
              {q || shown.length < rows.length ? "Try another name, roll number or filter." : "Students appear here once an admission is confirmed and the student is created."}
            </EmptyState>
          }
          columns={[
            {
              key: "name",
              header: "Name",
              render: (s) => (
                <Link to={student360Path(s.id)} className="st-name-link" data-student={s.studentNumber}>
                  <NameCell name={s.displayName} sub={s.status !== "active" ? humanize(s.status) : undefined} />
                </Link>
              ),
            },
            { key: "roll", header: "Roll no.", render: (s) => s.currentContext?.rollNumber ?? s.studentNumber, muted: true },
            { key: "prog", header: "Programme", render: (s) => progName(s.currentContext?.programmeId), muted: true },
            { key: "sem", header: "Semester", render: (s) => (s.currentContext?.currentTermNo ? `Sem ${s.currentContext.currentTermNo}` : "—") },
            {
              key: "att",
              header: "Attendance",
              render: (s) => {
                const o = overallAttendance(attendance.data?.get(s.id));
                return <AttendanceBadge percent={o?.percent} threshold={o?.threshold} />;
              },
            },
            { key: "fee", header: "Fee status", render: (s) => <FeeBadge status={feeOf(s)} /> },
          ]}
        />
      </BentoTile>
    </>
  );
}

/** "New admission" — prototype 3-step dialog (Applicant details → Programme & documents → Confirm). */
export function NewAdmissionDialog({ institution, open, onClose }: { institution: Institution; open: boolean; onClose: () => void }) {
  const api = useRecordApi();
  const { submit } = useRecordCommand();
  const { programmes } = useProgrammeNames(institution.id);
  const published = (programmes.data ?? []).filter((p) => p.status === "published");
  const [step, setStep] = useState(0);
  const [values, setValues] = useState<FormValues>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const formRef = useRef<HTMLFormElement | null>(null);
  if (!open) return null;
  const programmeId = values.programmeId || published[0]?.id || "";
  const steps: FieldSpec[][] = [
    [
      { name: "fullName", label: "Full name", required: true, placeholder: "As on the Class 12 marksheet", wide: true, member: ["givenName", "familyName"] },
      { name: "dateOfBirth", label: "Date of birth", type: "date", required: true, rule: "dob" },
      { name: "phone", label: "Mobile", type: "tel", placeholder: "98765 43210", rule: "mobile" },
      { name: "email", label: "Email", type: "email", placeholder: "name@example.com", wide: true, rule: "email" },
    ],
    [
      { name: "programmeId", label: "Programme", type: "select", required: true, options: published.map((p) => ({ value: p.id, label: p.name })), wide: true, initial: programmeId },
      { name: "subjectRef", label: "Self-service login (optional)", placeholder: "Login the applicant signs in with", hint: "Links the student portal once the student record is created.", wide: true },
    ],
  ];
  const close = () => {
    setStep(0);
    setValues({});
    setErrors({});
    setError(null);
    onClose();
  };
  const current = steps[step] ?? [];
  const next = async () => {
    // Phase B shared form layer: required + rules on this step, first invalid control focused.
    const missing = validateFields(current, { ...values, programmeId });
    setErrors(missing);
    if (Object.keys(missing).length) return void focusFirstInvalid(formRef.current, current.map((f) => f.name), missing);
    if (step < 2) return setStep(step + 1);
    const name = (values.fullName ?? "").trim().replace(/\s+/g, " ");
    const cut = name.lastIndexOf(" ");
    setBusy(true);
    setError(null);
    try {
      await submit(
        () =>
          api.createApplication(institution.id, {
            programmeId,
            givenName: cut > 0 ? name.slice(0, cut) : name,
            familyName: cut > 0 ? name.slice(cut + 1) : null,
            dateOfBirth: values.dateOfBirth || null,
            email: values.email || null,
            phone: values.phone || null,
            subjectRef: values.subjectRef || null,
          }),
        "Application recorded",
      );
      close();
    } catch (e) {
      // 422 → back to the step that holds the first offending field, error shown there.
      const all = steps.flat();
      const { fields: fe, unmatched } = problemFieldErrors(e, all.map((f) => f.name), memberAliases(all));
      const at = steps.findIndex((st) => st.some((f) => fe[f.name]));
      if (at >= 0) {
        setErrors(fe);
        setStep(at);
        queueMicrotask(() => focusFirstInvalid(formRef.current, all.map((f) => f.name), fe));
      }
      if (unmatched.length || at < 0) setError(unmatched[0] ?? problemMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const progName = published.find((p) => p.id === programmeId)?.name ?? "—";
  return (
    <DialogFrame open onClose={close} eyebrow="Students · New admission" title={["Applicant details", "Programme & documents", "Fee & confirm"][step]!} steps={["Applicant details", "Programme & documents", "Fee & confirm"]} step={step}>
      <form
        ref={formRef}
        data-form="new-application"
        data-step={step}
        noValidate
        style={{ display: "flex", flexDirection: "column", gap: 16 }}
        onSubmit={(e) => {
          e.preventDefault();
          void next();
        }}
      >
        {step < 2 ? (
          <div className="sb-fgrid" style={{ ["--cols" as string]: 2 }}>
            {current.map((f) => (
              <SpecField key={f.name} f={f} value={f.name === "programmeId" ? programmeId : (values[f.name] ?? "")} error={errors[f.name]}
                onChange={(v) => setValues((s) => ({ ...s, [f.name]: v }))}
                onBlur={() => setErrors((e) => { const m = validateField(f, f.name === "programmeId" ? programmeId : values[f.name]); const n = { ...e }; if (m) n[f.name] = m; else delete n[f.name]; return n; })}
              />
            ))}
          </div>
        ) : (
          <>
            <dl className="sb-dl st-confirm" data-confirm="">
              <dt>Applicant</dt>
              <dd>{values.fullName}</dd>
              <dt>Date of birth</dt>
              <dd>{formatDate(values.dateOfBirth)}</dd>
              <dt>Mobile</dt>
              <dd>{values.phone || "—"}</dd>
              <dt>Email</dt>
              <dd>{values.email || "—"}</dd>
              <dt>Programme</dt>
              <dd>{progName}</dd>
              <dt>Self-service login</dt>
              <dd>{values.subjectRef ? "Will be linked" : "Not linked"}</dd>
            </dl>
            <p className="st-meta">The application is recorded as a draft. Fees are raised once the admission is confirmed; required documents are set with the offer.</p>
          </>
        )}
        {error && (
          <p className="sb-field-error" role="alert">
            {error}
          </p>
        )}
        <div className="sb-dialog-foot">
          {step > 0 && (
            <button type="button" className="sb-btn sb-btn--ghost" onClick={() => setStep(step - 1)} disabled={busy}>
              Back
            </button>
          )}
          <button type="submit" className="sb-btn" disabled={busy || published.length === 0} data-dialog-primary="" data-step-next={step}>
            {busy ? "Working…" : step < 2 ? "Next" : "Record application"}
            {!busy && step < 2 && <ArrowRight strokeWidth={1.75} aria-hidden="true" />}
          </button>
          <button type="button" className="sb-btn sb-btn--ghost" onClick={close} disabled={busy}>
            Cancel
          </button>
        </div>
      </form>
    </DialogFrame>
  );
}

export function AdmissionsTab({ institution }: { institution: Institution | null }) {
  const api = useRecordApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const [docsFor, setDocsFor] = useState<Admission | null>(null);
  const inst = institution?.id ?? "-";
  const apps = useQuery({ queryKey: rkeys.scoped(inst, "applications"), queryFn: () => api.listApplications(institution!.id), enabled: !!institution });
  const adms = useQuery({ queryKey: rkeys.scoped(inst, "admissions"), queryFn: () => api.listAdmissions(institution!.id), enabled: !!institution });
  const { programmes, name: progName } = useProgrammeNames(institution?.id);
  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const published = (programmes.data ?? []).filter((p) => p.status === "published");

  const appActions = (a: AdmissionApplication): RowAction[] => {
    const out: RowAction[] = [];
    if (a.status === "draft") out.push({ id: "application-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.applicationCommand(a.id, a.version, "submit"), "Application submitted") });
    if (a.status === "submitted") out.push({ id: "application-review", label: "Start review", variant: "primary", onClick: () => void run(() => api.applicationCommand(a.id, a.version, "review"), "Review started") });
    if (a.status === "submitted" || a.status === "under_review") {
      out.push({
        id: "application-offer",
        label: "Offer admission",
        variant: a.status === "under_review" ? "primary" : undefined,
        onClick: () =>
          ask({
            title: `Offer admission to ${a.displayName}`,
            eyebrow: "Admissions · Offer",
            name: "offer",
            submitLabel: "Make offer",
            fields: [
              { name: "offerExpiresOn", label: "Offer valid until", type: "date" },
              { name: "requiredDocuments", label: "Documents to collect", type: "tags", options: ADMISSION_DOCUMENTS.map((d) => ({ value: d, label: humanize(d) })), wide: true },
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
      out.push({ id: "application-reject", label: "Reject", onClick: () => ask({ title: "Reject application", eyebrow: "Admissions · Reject", name: "reject-application", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.applicationCommand(a.id, a.version, "reject", v.reason), "Application rejected") }) });
    }
    if (["draft", "submitted", "under_review"].includes(a.status))
      out.push({ id: "application-withdraw", label: "Withdraw", onClick: () => ask({ title: "Withdraw application", eyebrow: "Admissions · Withdraw", name: "withdraw-application", submitLabel: "Withdraw", fields: [REASON], onSubmit: (v) => submit(() => api.applicationCommand(a.id, a.version, "withdraw", v.reason), "Application withdrawn") }) });
    return out;
  };

  const admAction = (a: Admission, k: ReturnType<typeof admissionActions>[number]): RowAction => {
    const label = ADMISSION_LABEL[k] ?? humanize(k);
    const onClick = () => {
      if (k === "documents") return setDocsFor(a);
      if (k === "decide")
        return ask({
          title: `Approval decision — ${a.displayName}`,
          eyebrow: "Admissions · Decision",
          name: "admission-decision",
          submitLabel: "Record decision",
          fields: [
            { name: "decision", label: "Decision", type: "select", initial: "approve", options: [{ value: "approve", label: "Approve" }, { value: "reject", label: "Reject" }], wide: true },
            { name: "reason", label: "Reason (required to reject)", type: "textarea", wide: true },
          ],
          onSubmit: (v) => submit(() => api.decideAdmission(a.id, a.version, v.decision === "reject" ? "reject" : "approve", v.reason || undefined), "Decision recorded"),
        });
      if (k === "revoke")
        return ask({
          title: `Revoke admission — ${a.displayName}`,
          eyebrow: "Admissions · Revoke",
          name: "admission-revoke",
          submitLabel: "Revoke",
          fields: [REASON, { name: "evidence", label: "Supporting evidence", required: true, wide: true }],
          onSubmit: (v) => submit(() => api.revokeAdmission(a.id, a.version, v.reason!, v.evidence!), "Admission revoked"),
        });
      if (k === "readmit") return ask({ title: `Readmit ${a.displayName}`, eyebrow: "Admissions · Readmission", name: "admission-readmit", submitLabel: "Start readmission", fields: [REASON], onSubmit: (v) => submit(() => api.readmit(a.id, v.reason!), "Readmission started — a new admission was initiated") });
      const cmd = k as AdmissionCommand;
      if (NEEDS_REASON.has(cmd)) return ask({ title: `${label} — ${a.displayName}`, eyebrow: "Admissions", name: `admission-${cmd}`, submitLabel: label, fields: [REASON], onSubmit: (v) => submit(() => api.admissionCommand(a.id, a.version, cmd, v.reason), `${label} done`) });
      void run(() => api.admissionCommand(a.id, a.version, cmd), `${label} done`);
    };
    return { id: `admission-${k}`, label, onClick, variant: k === "documents" || NEEDS_REASON.has(k) || k === "expire" || k === "revoke" ? undefined : ("primary" as const) };
  };

  const applications = apps.data ?? [];
  const admissions = adms.data ?? [];
  return (
    <>
      <AdmissionFunnelTile institutionId={institution.id} programmes={(programmes.data ?? []).map((p) => ({ id: p.id, name: p.name }))} />
      <BentoTile span={12} title="Applications" action={<span className="st-count">{`${applications.length.toLocaleString("en-IN")} total`}</span>}>
        {applications.length === 0 ? (
          <EmptyState icon={UserPlus} title="No applications yet" compact>
            {published.length === 0 ? "Publish a programme in Academics first." : "Use New admission to record the first application."}
          </EmptyState>
        ) : (
          <div className="st-lrows">
            {applications.map((a) => (
              <ListRow
                key={a.id}
                data-application={a.displayName}
                avatar={a.displayName}
                title={a.displayName}
                meta={[progName(a.programmeId), a.createdAt ? `applied ${formatDate(a.createdAt.slice(0, 10))}` : null].filter(Boolean).join(" · ")}
                badge={{ label: humanize(a.status), kind: statusKindOf(a.status) }}
                {...rowProps(appActions(a), busy)}
              />
            ))}
          </div>
        )}
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
        {admissions.length === 0 ? (
          <EmptyState title="No admission records" compact>
            An offer on an application creates the admission record.
          </EmptyState>
        ) : (
          <div className="st-lrows">
            {admissions.map((a) => (
              <ListRow
                key={a.id}
                data-admission={a.displayName}
                avatar={a.displayName}
                title={a.studentId ? <Link to={student360Path(a.studentId)} className="st-name-link">{a.displayName}</Link> : a.displayName}
                meta={[progName(a.programmeId), a.offerExpiresOn ? `offer valid until ${formatDate(a.offerExpiresOn)}` : null, a.offerAcceptedAt ? "accepted" : null, humanize(a.enrolmentType)].filter(Boolean).join(" · ")}
                badge={{ label: humanize(a.status), kind: statusKindOf(a.status) }}
                {...rowProps(admissionActions(a).map((k) => admAction(a, k)), busy)}
              />
            ))}
          </div>
        )}
      </BentoTile>
      <AdmissionDocumentsPanel admission={docsFor} onClose={() => setDocsFor(null)} />
      {prompt}
      {dialog}
    </>
  );
}

function statusKindOf(s: string) {
  return (["approved", "confirmed", "student_created", "completed", "verification_completed", "financially_cleared"].includes(s)
    ? "done"
    : ["rejected", "cancelled", "withdrawn", "revoked", "expired"].includes(s)
      ? "attention"
      : ["submitted", "under_review", "approval_pending", "documents_submitted"].includes(s)
        ? "review"
        : s === "draft"
          ? "neutral"
          : "info") as "done" | "attention" | "review" | "neutral" | "info";
}

function AdmissionDocumentsPanel({ admission, onClose }: { admission: Admission | null; onClose: () => void }) {
  const api = useRecordApi();
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const id = admission?.id ?? "-";
  const docs = useQuery({ queryKey: rkeys.scoped("admission", id, "documents"), queryFn: () => api.admissionDocuments(admission!.id), enabled: !!admission });
  // Part 2 · S4 (UI docs): file upload / replace / download on the admission documents.
  const client = useQueryClient();
  const toast = useToast();
  const files = useDownload();
  const [upload, setUpload] = useState<{ doc: AdmissionDocument; replace: boolean } | null>(null);
  if (!admission) return null;
  const canVerify = admission.status === "documents_submitted";
  const canAdd = ["initiated", "documents_submitted"].includes(admission.status);
  const list = docs.data ?? [];
  return (
    <SidePanel
      open
      onClose={onClose}
      title={admission.displayName}
      eyebrow="Admissions · Documents"
      chip={{ label: humanize(admission.status), kind: statusKindOf(admission.status) }}
      facts={[
        { label: "Required", value: admission.requiredDocuments.map((d) => humanize(d)).join(", ") || "None" },
        { label: "Handed in", value: `${list.length} of ${Math.max(list.length, admission.requiredDocuments.length)}` },
        { label: "Verified", value: String(list.filter((d) => d.status === "verified" || d.status === "waived").length) },
      ]}
      actions={
        <button
          type="button"
          className="sb-btn"
          disabled={!canAdd}
          data-add-admission-document
          onClick={() =>
            ask({
              title: "Hand in a document",
              eyebrow: "Admissions · Documents",
              name: "admission-document",
              submitLabel: "Add document",
              fields: [
                { name: "documentType", label: "Type", type: "select", initial: admission.requiredDocuments[0] ?? "identity_proof", options: [...new Set([...admission.requiredDocuments, "photo", "transfer_certificate"])].map((t) => ({ value: t, label: humanize(t) })), wide: true },
                { name: "title", label: "Title", required: true, placeholder: "Aadhaar card" },
                { name: "fileRef", label: "Storage reference", placeholder: "Document store link" },
              ],
              onSubmit: (v) => submit(() => api.addAdmissionDocument(admission.id, { documentType: v.documentType!, title: v.title!.trim(), fileRef: v.fileRef || undefined }), "Document added"),
            })
          }
        >
          <FilePlus2 strokeWidth={1.75} aria-hidden="true" />
          Add document
        </button>
      }
    >
      {list.length === 0 ? (
        <EmptyState title="No documents yet" compact>
          Add each required document.
        </EmptyState>
      ) : (
        <div className="st-lrows">
          {list.map((d) => (
            <ListRow
              key={d.id}
              data-admission-doc={d.documentType}
              title={humanize(d.documentType)}
              data-file={d.documentObjectId ? "attached" : "none"}
              meta={[d.title, d.documentObjectId ? "File attached" : d.status === "submitted" ? "No file yet" : null].filter(Boolean).join(" · ")}
              badge={{ label: humanize(d.status), kind: d.status === "verified" || d.status === "waived" ? "done" : d.status === "rejected" ? "attention" : "review" }}
              {...rowProps(
                [
                  ...(canVerify && d.status === "submitted"
                    ? [
                        { id: "doc-verify", label: "Verify", variant: "primary" as const, onClick: () => void run(() => api.verifyAdmissionDocument(admission.id, d.id, d.version, "verified"), "Document verified") },
                        { id: "doc-reject", label: "Reject", onClick: () => ask({ title: "Reject document", eyebrow: "Admissions · Documents", name: "doc-reject", submitLabel: "Reject", fields: [{ name: "note", label: "Why", required: true, wide: true, type: "textarea" }], onSubmit: (v) => submit(() => api.verifyAdmissionDocument(admission.id, d.id, d.version, "rejected", v.note), "Document rejected") }) },
                        { id: "doc-waive", label: "Waive", onClick: () => ask({ title: "Waive document", eyebrow: "Admissions · Documents", name: "doc-waive", submitLabel: "Waive", fields: [{ name: "note", label: "Why", required: true, wide: true, type: "textarea" }], onSubmit: (v) => submit(() => api.verifyAdmissionDocument(admission.id, d.id, d.version, "waived", v.note), "Document waived") }) },
                      ]
                    : []),
                  ...(canAdd && d.status === "submitted" && !d.documentObjectId ? [{ id: "doc-upload", label: "Upload file", variant: "primary" as const, onClick: () => setUpload({ doc: d, replace: false }) }] : []),
                  ...(canAdd && d.status === "rejected" ? [{ id: "doc-replace", label: "Replace", variant: "primary" as const, onClick: () => setUpload({ doc: d, replace: true }) }] : []),
                  ...(d.documentObjectId ? [{ id: "doc-download", label: "Download", onClick: () => void files.download(() => api.downloadAdmissionFile(admission.id, d.id)) }] : []),
                ],
                busy || files.busy,
              )}
            />
          ))}
        </div>
      )}
      {upload && (
        <DocumentUploadDialog
          title={`${upload.replace ? "Replace" : "Upload"} ${humanize(upload.doc.documentType).toLowerCase()}`}
          eyebrow={`Admissions · ${admission.displayName}`}
          replace={upload.replace}
          onUpload={async (file) => {
            // Replace = hand in a new document of the same type (the server supersedes the rejected one), then attach the file.
            const target = upload.replace ? await api.addAdmissionDocument(admission.id, { documentType: upload.doc.documentType, title: upload.doc.title }) : upload.doc;
            return api.uploadAdmissionFile(admission.id, target.id, file);
          }}
          onClose={() => setUpload(null)}
          onDone={() => {
            setUpload(null);
            toast("File uploaded");
            void client.invalidateQueries({ queryKey: rkeys.all });
          }}
        />
      )}
      {prompt}
      {dialog}
    </SidePanel>
  );
}

