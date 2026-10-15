/**
 * Student 360 tabs (re-skin v2, R2). Prototype header tabs: Overview ("At a glance" stat cells +
 * Contact), Academics (current courses + past semesters), Attendance (by course), Fees (annual fee
 * + payments) and Documents (list rows). Lifecycle sections (enrolment, …) live under "More".
 * Record actions (profile, identity correction, suspension / restoration, archival) are menu
 * items of the header (`useStudentRecordActions`). Faculty see everything read-only.
 */
import { BookOpen, FileText, History } from "lucide-react";
import {
  BentoTile,
  DataTable,
  EmptyState,
  ListRow,
  ProgressBar,
  StatCells,
  StatusBadge,
  formatDate,
  formatDateTime,
  formatINR,
  formatNumber,
  humanize,
} from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { useSession } from "../../../lib/auth";
import { useInstitutionApi, useInstitutions, keys as instKeys } from "../../institution/hooks";
import { rkeys as regKeys, useRegistrationApi } from "../registration/hooks";
import { Actions, DialogFrame, REASON, rowProps, SpecForm, useAsk, type RowAction } from "../ui";
import { AttendanceBadge, FeeBadge, useProgrammeNames } from "./AdminTabs";
import type { Student } from "./api";
import { rkeys, useRecordApi, useRecordCommand } from "./hooks";
import { feeStatus, overallAttendance, useAttendanceIndex, useFeeAccounts, useFeeDetail } from "./insights";

export const REQUEST_TYPES = [
  "bonafide_certificate",
  "transcript_copy",
  "migration_certificate",
  "character_certificate",
  "conduct_certificate",
  "enrolment_letter",
  "id_card_reissue",
] as const;

export const humanise = (s: string) => humanize(s);

export function useStudent(studentId: string) {
  const api = useRecordApi();
  return useQuery({ queryKey: rkeys.scoped("student", studentId), queryFn: () => api.getStudent(studentId), enabled: !!studentId });
}

/** Label → value facts in the prototype `dl` style. */
export function Facts({ rows }: { rows: Array<[string, ReactNode]> }) {
  return (
    <dl className="sb-dl">
      {rows.map(([k, v]) => (
        <FactPair key={k} label={k} value={v} />
      ))}
    </dl>
  );
}
function FactPair({ label, value }: { label: string; value: ReactNode }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value ?? "—"}</dd>
    </>
  );
}

function Loading({ q, children }: { q: { isLoading: boolean; error: unknown; data?: Student }; children: (s: Student) => ReactNode }) {
  if (q.isLoading)
    return (
      <BentoTile span={12}>
        <p className="st-meta">Loading student…</p>
      </BentoTile>
    );
  if (!q.data)
    return (
      <BentoTile span={12}>
        <EmptyState title="Student not found">The record does not exist or belongs to another tenant.</EmptyState>
      </BentoTile>
    );
  return <>{children(q.data)}</>;
}

/** Cross-domain figures of one student (administrators only: finance / academic reads). */
export function useStudentFigures(s: Student | undefined) {
  const { role } = useSession();
  const admin = role === "administrator";
  const inst = s?.currentContext?.institutionId ?? s?.contexts[0]?.institutionId ?? null;
  const fees = useFeeAccounts(inst, admin);
  const attendance = useAttendanceIndex(inst, admin);
  const account = s ? fees.data?.get(s.id) : undefined;
  const courses = s ? attendance.data?.get(s.id) : undefined;
  return { admin, inst, account, fee: feeStatus(account), courses, attendance: overallAttendance(courses) };
}

// ================================================================== record actions (header "More")

/** The administrator's record commands of a student, as header menu items + their dialogs. */
export function useStudentRecordActions(s: Student | undefined) {
  const api = useRecordApi();
  const { role } = useSession();
  const { submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const actions: RowAction[] = [];
  if (s && role === "administrator" && s.status !== "archived") {
    const c = s.currentContext;
    actions.push({
      id: "student-profile",
      label: "Edit profile",
      onClick: () =>
        ask({
          title: "Edit profile",
          eyebrow: `Students · ${s.displayName}`,
          name: "student-profile",
          submitLabel: "Save",
          fields: [
            { name: "preferredName", label: "Preferred name", initial: s.preferredName ?? "", wide: true },
            { name: "primaryEmail", label: "Email", type: "email", initial: s.primaryEmail ?? "" },
            { name: "primaryPhone", label: "Mobile", type: "tel", initial: s.primaryPhone ?? "" },
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
          eyebrow: `Students · ${s.displayName}`,
          name: "student-identity",
          submitLabel: "Submit for approval",
          fields: [
            { name: "givenName", label: "Given name", initial: s.givenName },
            { name: "familyName", label: "Family name", initial: s.familyName ?? "" },
            { name: "dateOfBirth", label: "Date of birth", type: "date", initial: s.dateOfBirth ?? "", wide: true },
            { name: "evidence", label: "Supporting evidence", required: true, wide: true, type: "textarea", placeholder: "Which document shows the correct identity" },
          ],
          onSubmit: (v) =>
            submit(
              async () => {
                const changes: Record<string, string> = {};
                if (v.givenName && v.givenName !== s.givenName) changes.givenName = v.givenName;
                if ((v.familyName ?? "") !== (s.familyName ?? "")) changes.familyName = v.familyName ?? "";
                if (v.dateOfBirth && v.dateOfBirth !== s.dateOfBirth) changes.dateOfBirth = v.dateOfBirth;
                const ic = await api.requestCorrection(s.id, changes, v.evidence!);
                return api.submitCorrection(ic.id, ic.version);
              },
              (r) => (r.status === "completed" ? "Identity corrected" : "Correction sent for approval"),
            ),
        }),
    });
    if (c?.status === "active")
      actions.push({
        id: "student-suspend",
        label: "Suspend",
        danger: true,
        onClick: () =>
          ask({
            title: "Suspend student",
            eyebrow: `Students · ${s.displayName}`,
            name: "student-suspend",
            submitLabel: "Suspend",
            fields: [{ name: "category", label: "Category", type: "select", initial: "administrative", wide: true, options: ["administrative", "disciplinary", "financial", "academic", "medical", "other"].map((x) => ({ value: x, label: humanize(x) })) }, REASON],
            onSubmit: (v) => submit(() => api.suspend(s.id, c.version, v.reason!, v.category!), "Student suspended"),
          }),
      });
    if (c?.status === "suspended")
      actions.push({ id: "student-restore", label: "Restore", variant: "primary", onClick: () => ask({ title: "Restore student", eyebrow: `Students · ${s.displayName}`, name: "student-restore", submitLabel: "Restore", fields: [REASON], onSubmit: (v) => submit(() => api.restore(s.id, c.version, v.reason!), "Student restored") }) });
    if (!s.contexts.some((x) => ["pending_activation", "active", "suspended"].includes(x.status)) && s.contexts.length > 0)
      actions.push({ id: "student-archive", label: "Archive record", danger: true, onClick: () => ask({ title: "Archive the student record", eyebrow: `Students · ${s.displayName}`, name: "student-archive", submitLabel: "Archive", fields: [REASON], onSubmit: (v) => submit(() => api.archive(s.id, s.version, v.reason!), "Record archived") }) });
  }
  return { actions, busy, ui: (
    <>
      {prompt}
      {dialog}
    </>
  ) };
}

// ================================================================== overview

export function Student360Overview({ studentId }: { studentId: string }) {
  const api = useRecordApi();
  const q = useStudent(studentId);
  const history = useQuery({ queryKey: rkeys.scoped("student", studentId, "status-history"), queryFn: () => api.statusHistory(studentId), enabled: !!studentId });
  const reg = useRegistrationApi();
  const decisions = useQuery({ queryKey: regKeys.scoped(studentId, "decisions"), queryFn: () => reg.listDecisions(studentId), enabled: !!studentId });
  const fig = useStudentFigures(q.data);
  const { name: progName } = useProgrammeNames(fig.inst);
  return (
    <Loading q={q}>
      {(s) => {
        const c = s.currentContext;
        const latest = [...(decisions.data ?? [])].filter((d) => d.status === "effective" && d.cgpa != null).sort((a, b) => b.termNo - a.termNo)[0];
        const charged = Number(fig.account?.balance.totalCharged ?? 0);
        const settled = Number(fig.account?.balance.totalSettled ?? 0);
        const att = fig.attendance;
        const rows = [...(history.data ?? [])].reverse();
        return (
          <>
            <BentoTile span={8} title="At a glance" data-glance="">
              <StatCells
                columns={2}
                cells={[
                  {
                    label: "Attendance",
                    value: att ? `${Math.round(att.percent)}%` : "—",
                    sub: att ? (att.below ? `Below the ${formatNumber(att.threshold)}% minimum` : `Across ${fig.courses?.length ?? 0} ${fig.courses?.length === 1 ? "course" : "courses"}`) : "No finalised sessions yet",
                    kind: att ? (att.below ? "attention" : "done") : undefined,
                  },
                  { label: "CGPA", value: latest?.cgpa != null ? formatNumber(latest.cgpa, 2) : "—", sub: latest ? `After semester ${latest.termNo}` : c?.currentTermNo && c.currentTermNo <= 1 ? "First semester" : "No results yet" },
                  {
                    label: "Fees paid",
                    value: charged > 0 ? `${Math.round((settled / charged) * 100)}%` : "—",
                    sub: charged > 0 ? `${formatINR(settled)} of ${formatINR(charged)}` : "No fees raised yet",
                    kind: fig.fee === "overdue" ? "attention" : fig.fee === "paid" ? "done" : undefined,
                  },
                  { label: "Semester", value: c?.currentTermNo ? String(c.currentTermNo) : "—", sub: progName(c?.programmeId) },
                ]}
              />
            </BentoTile>
            <BentoTile span={4} title="Contact" data-current-context={c?.status ?? "none"}>
              <Facts
                rows={[
                  ["Roll no.", c?.rollNumber ?? s.studentNumber],
                  ...(c?.rollNumber ? ([["Student no.", s.studentNumber]] as Array<[string, ReactNode]>) : []),
                  ["Email", s.primaryEmail],
                  ["Mobile", s.primaryPhone],
                  ["Date of birth", s.dateOfBirth ? formatDate(s.dateOfBirth) : null],
                  ["Admitted", c?.enrolledOn ? new Intl.DateTimeFormat("en-IN", { month: "short", year: "numeric", timeZone: "UTC" }).format(new Date(`${c.enrolledOn}T00:00:00Z`)) : null],
                  ["Batch · section", [c?.batchCode, c?.sectionCode].filter(Boolean).join(" · ") || null],
                  ["Enrolment", c ? <StatusBadge key="s" status={c.status} /> : "No enrolment"],
                  ["Self-service", s.subjectLinked ? "Login linked" : "Not linked"],
                  ...(s.preferredName ? ([["Preferred name", s.preferredName]] as Array<[string, ReactNode]>) : []),
                ]}
              />
            </BentoTile>
            <BentoTile span={12} title="Status history" action={rows.length ? <span className="st-count">{`${rows.length} ${rows.length === 1 ? "change" : "changes"}`}</span> : undefined}>
              {rows.length === 0 ? (
                <EmptyState icon={History} title="No changes yet" compact>
                  Status changes appear here.
                </EmptyState>
              ) : (
                <div className="st-lrows">
                  {rows.map((h) => (
                    <ListRow
                      key={`${h.occurredAt}-${h.scope}-${h.toStatus}`}
                      icon={History}
                      tone="grey"
                      title={`${h.scope === "context" ? "Enrolment" : "Student"}: ${humanize(h.fromStatus ?? "new")} → ${humanize(h.toStatus)}`}
                      meta={[formatDateTime(h.occurredAt), h.reason].filter(Boolean).join(" · ")}
                      badge={{ label: humanize(h.toStatus), kind: badgeKind(h.toStatus) }}
                    />
                  ))}
                </div>
              )}
            </BentoTile>
          </>
        );
      }}
    </Loading>
  );
}

function badgeKind(s: string): "done" | "attention" | "review" | "neutral" | "info" {
  if (["active", "verified", "issued", "completed", "effective", "registered", "finalised"].includes(s)) return "done";
  if (["suspended", "rejected", "cancelled", "withdrawn", "archived", "discontinued"].includes(s)) return "attention";
  if (["pending_activation", "requested", "submitted", "in_progress"].includes(s)) return "review";
  return "neutral";
}

// ================================================================== academics

export function Student360Academics({ studentId }: { studentId: string }) {
  const reg = useRegistrationApi();
  const q = useStudent(studentId);
  const regs = useQuery({ queryKey: regKeys.scoped(studentId, "registrations"), queryFn: () => reg.listRegistrations(studentId), enabled: !!studentId });
  const decisions = useQuery({ queryKey: regKeys.scoped(studentId, "decisions"), queryFn: () => reg.listDecisions(studentId), enabled: !!studentId });
  return (
    <Loading q={q}>
      {(s) => {
        const current = [...(regs.data ?? [])].filter((r) => r.status !== "cancelled").sort((a, b) => b.termNo - a.termNo)[0];
        const lines = (current?.lines ?? []).filter((l) => l.status !== "dropped");
        const past = [...(decisions.data ?? [])].filter((d) => d.status === "effective").sort((a, b) => b.termNo - a.termNo);
        const term = current?.termNo ?? s.currentContext?.currentTermNo;
        return (
          <>
            <BentoTile span={8} title={term ? `Semester ${term} · current courses` : "Current courses"} data-academics="">
              <DataTable
                caption="Current courses"
                rows={lines}
                rowKey={(l) => l.id}
                empty={
                  <EmptyState icon={BookOpen} title="No registered courses" compact>
                    Courses appear once the student registers for the term.
                  </EmptyState>
                }
                columns={[
                  {
                    key: "course",
                    header: "Course",
                    render: (l) => (
                      <span>
                        <b>{l.title}</b> <small className="st-count">{l.slotCode}</small>
                      </span>
                    ),
                  },
                  { key: "cr", header: "Credits", render: (l) => formatNumber(l.credits) },
                  { key: "kind", header: "Type", render: (l) => humanize(l.kind), muted: true },
                  { key: "st", header: "Status", render: (l) => <StatusBadge status={l.status} /> },
                ]}
              />
              {current && <p className="st-meta" style={{ marginTop: 12 }}>{`${formatNumber(lines.reduce((a, l) => a + l.credits, 0))} credits · registration ${humanize(current.status).toLowerCase()}`}</p>}
            </BentoTile>
            <BentoTile span={4} title="Past semesters">
              {past.length === 0 ? (
                <p className="st-meta">{term && term <= 1 ? "First semester, no results yet." : "No semester results recorded yet."}</p>
              ) : (
                <div className="st-lrows">
                  {past.map((d) => (
                    <ListRow
                      key={d.id}
                      title={`Semester ${d.termNo}`}
                      meta={[d.sgpa != null ? `SGPA ${formatNumber(d.sgpa, 2)}` : null, d.cgpa != null ? `CGPA ${formatNumber(d.cgpa, 2)}` : null, `${formatNumber(d.creditsEarned)} credits earned`].filter(Boolean).join(" · ")}
                      badge={{ label: humanize(d.outcome), kind: d.outcome === "progressed" || d.outcome === "qualification_eligible" ? "done" : d.outcome === "detained" ? "attention" : "review" }}
                    />
                  ))}
                </div>
              )}
            </BentoTile>
          </>
        );
      }}
    </Loading>
  );
}

// ================================================================== attendance

export function Student360Attendance({ studentId }: { studentId: string }) {
  const q = useStudent(studentId);
  const fig = useStudentFigures(q.data);
  return (
    <Loading q={q}>
      {() => {
        const courses = fig.courses ?? [];
        const threshold = courses.length ? Math.max(...courses.map((c) => c.threshold)) : 75;
        return (
          <BentoTile span={12} title="Attendance by course" data-attendance="" action={<span className="st-count">{`Finalised sessions · minimum ${formatNumber(threshold)}%`}</span>}>
            <DataTable
              caption="Attendance by course"
              rows={courses}
              rowKey={(c) => `${c.offeringId}-${c.sectionCode}`}
              empty={
                <EmptyState icon={BookOpen} title="No attendance recorded yet" compact>
                  {fig.admin ? "Percentages appear once teachers finalise their sessions." : "Attendance is visible to administrators."}
                </EmptyState>
              }
              columns={[
                {
                  key: "course",
                  header: "Course",
                  render: (c) => (
                    <span>
                      <b>{c.title}</b> <small className="st-count">{`${c.courseCode} · section ${c.sectionCode}`}</small>
                    </span>
                  ),
                },
                { key: "held", header: "Held", render: (c) => formatNumber(c.line.total) },
                { key: "att", header: "Attended", render: (c) => formatNumber(c.line.attended), muted: true },
                { key: "pc", header: "Attendance", render: (c) => <AttendanceBadge percent={c.line.percent} threshold={c.threshold} /> },
                { key: "bar", header: "", render: (c) => <div style={{ minWidth: 220 }}><ProgressBar value={Math.min(100, c.line.percent)} kind={c.line.belowThreshold ? "attention" : "accent"} /></div> },
              ]}
            />
          </BentoTile>
        );
      }}
    </Loading>
  );
}

// ================================================================== fees

const CHANNEL: Record<string, string> = { gateway: "Online", cash: "Cash", cheque: "Cheque", bank_transfer: "Bank transfer", demand_draft: "Demand draft", sponsor: "Sponsor" };

export function Student360Fees({ studentId }: { studentId: string }) {
  const q = useStudent(studentId);
  const fig = useStudentFigures(q.data);
  const { charges, payments } = useFeeDetail(fig.account?.id);
  return (
    <Loading q={q}>
      {() => {
        const b = fig.account?.balance;
        const open = (charges.data ?? []).filter((c) => Number(c.outstandingAmount) > 0 && c.dueDate).sort((x, y) => (x.dueDate ?? "").localeCompare(y.dueDate ?? ""));
        const paid = (payments.data ?? []).filter((p) => p.status === "completed");
        return (
          <>
            <BentoTile span={4} title="Fee account" data-fees="">
              {b ? (
                <Facts
                  rows={[
                    ["Charged", formatINR(b.totalCharged)],
                    ...(Number(b.totalAdjusted) !== 0 ? ([["Adjusted", formatINR(b.totalAdjusted)]] as Array<[string, ReactNode]>) : []),
                    ["Paid", formatINR(b.totalSettled)],
                    ["Balance", formatINR(b.outstanding)],
                    ["Status", <FeeBadge key="f" status={fig.fee} />],
                    ["Due by", open[0]?.dueDate ? formatDate(open[0].dueDate) : "—"],
                  ]}
                />
              ) : (
                <EmptyState title="No fee account" compact>
                  {fig.admin ? "Fees appear once a charge is raised for the student." : "Fees are visible to administrators."}
                </EmptyState>
              )}
            </BentoTile>
            <BentoTile span={8} title="Payments">
              <DataTable
                caption="Payments"
                rows={paid}
                rowKey={(p) => p.id}
                empty={
                  <EmptyState title="No payments yet" compact>
                    Received payments and their receipts appear here.
                  </EmptyState>
                }
                columns={[
                  { key: "date", header: "Date", render: (p) => <b>{formatDate(p.receivedOn)}</b> },
                  { key: "mode", header: "Mode", render: (p) => CHANNEL[p.channel] ?? humanize(p.channel) },
                  { key: "ref", header: "Reference", render: (p) => p.receipt?.receiptNumber ?? p.paymentNumber, muted: true },
                  { key: "amt", header: "Amount", render: (p) => <span className="st-strong st-num">{formatINR(p.amount)}</span> },
                ]}
              />
              {(charges.data ?? []).length > 0 && (
                <div className="st-lrows" style={{ marginTop: 12 }}>
                  {(charges.data ?? []).map((c) => (
                    <ListRow key={c.id} icon={FileText} tone="blue" title={c.description} meta={[c.chargeNumber, c.dueDate ? `due ${formatDate(c.dueDate)}` : null].filter(Boolean).join(" · ")} value={formatINR(c.totalAmount)} badge={{ label: humanize(c.status), kind: Number(c.outstandingAmount) <= 0 && Number(c.totalAmount) > 0 ? "done" : Number(c.outstandingAmount) > 0 ? "review" : "neutral" }} />
                  ))}
                </div>
              )}
            </BentoTile>
          </>
        );
      }}
    </Loading>
  );
}

// ================================================================== enrolment (More)

/** Display names of the institutions and the programmes of `institutionId` (ids → names). */
function useNames(institutionId: string | null | undefined) {
  const instApi = useInstitutionApi();
  const { list } = useInstitutions();
  const programmes = useQuery({ queryKey: instKeys.scoped(institutionId ?? "-", "programmes"), queryFn: () => instApi.listProgrammes(institutionId!), enabled: !!institutionId });
  return {
    institutions: list,
    institution: (id: string | null | undefined) => list.find((i) => i.id === id)?.name ?? "Another institution",
    programme: (id: string | null | undefined) => programmes.data?.find((x) => x.id === id)?.name ?? "—",
  };
}

function TransferDialog({ s, onClose }: { s: Student; onClose: () => void }) {
  const api = useRecordApi();
  const instApi = useInstitutionApi();
  const { submit } = useRecordCommand();
  const { list } = useInstitutions();
  const c = s.currentContext!;
  const [kind, setKind] = useState("intra_institute");
  const others = list.filter((i) => i.id !== c.institutionId);
  const [dest, setDest] = useState(others[0]?.id ?? "");
  const progs = useQuery({ queryKey: instKeys.scoped(dest || "-", "programmes"), queryFn: () => instApi.listProgrammes(dest), enabled: kind === "inter_institute" && !!dest });
  const inter = kind === "inter_institute";
  return (
    <DialogFrame open onClose={onClose} eyebrow={`Students · ${s.displayName}`} title="Transfer the student">
      <SpecForm
        key={`${kind}-${dest}-${progs.data?.length ?? 0}`}
        name="student-transfer"
        submitLabel="Request + submit"
        onCancel={onClose}
        fields={[
          { name: "kind", label: "Kind", type: "select", initial: kind, wide: true, options: [{ value: "intra_institute", label: "Within this institution" }, ...(others.length ? [{ value: "inter_institute", label: "To another institution" }] : [])] },
          ...(inter
            ? [
                { name: "toInstitutionId", label: "Destination institution", type: "select" as const, initial: dest, options: others.map((i) => ({ value: i.id, label: i.name })) },
                { name: "toProgrammeId", label: "Destination programme", type: "select" as const, initial: progs.data?.[0]?.id ?? "", options: (progs.data ?? []).map((p) => ({ value: p.id, label: p.name })) },
              ]
            : []),
          { name: "toSectionCode", label: "Destination section", placeholder: "B" },
          REASON,
        ]}
        onSubmit={async (v) => {
          if (v.kind !== kind) {
            setKind(v.kind ?? "intra_institute");
            throw new Error("Pick the destination, then submit again.");
          }
          if (inter && v.toInstitutionId && v.toInstitutionId !== dest) {
            setDest(v.toInstitutionId);
            throw new Error("Pick the destination programme, then submit again.");
          }
          await submit(
            async () => {
              const t = await api.requestTransfer(s.id, {
                kind: inter ? "inter_institute" : "intra_institute",
                toInstitutionId: inter ? v.toInstitutionId || null : null,
                toProgrammeId: inter ? v.toProgrammeId || null : null,
                toSectionCode: v.toSectionCode || null,
                reason: v.reason!,
              });
              return api.submitTransfer(t.id, t.version);
            },
            (t) => (t.status === "completed" ? "Transfer completed" : "Transfer sent for approval"),
          );
          onClose();
        }}
      />
    </DialogFrame>
  );
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
  const [transferring, setTransferring] = useState(false);
  return (
    <Loading q={q}>
      {(s) => {
        const c = s.currentContext;
        const actions: RowAction[] = [];
        if (admin && c?.status === "active") {
          actions.push({
            id: "student-enrolment-update",
            label: "Update enrolment",
            onClick: () =>
              ask({
                title: "Update enrolment",
                eyebrow: `Students · ${s.displayName}`,
                name: "student-enrolment-update",
                submitLabel: "Save",
                fields: [
                  { name: "enrolmentNumber", label: "Enrolment number", initial: c.enrolmentNumber ?? "" },
                  { name: "rollNumber", label: "Roll number", initial: c.rollNumber ?? "" },
                  { name: "reason", label: "Reason", wide: true, type: "textarea" },
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
                eyebrow: `Students · ${s.displayName}`,
                name: "student-assignment",
                submitLabel: "Save",
                fields: [
                  { name: "batchCode", label: "Batch", initial: c.batchCode ?? "" },
                  { name: "sectionCode", label: "Section", initial: c.sectionCode ?? "" },
                  { name: "reason", label: "Reason", wide: true, type: "textarea" },
                ],
                onSubmit: (v) => submit(() => api.changeAssignment(s.id, c.version, { batchCode: v.batchCode, sectionCode: v.sectionCode, reason: v.reason || null }), "Assignment changed"),
              }),
          });
          actions.push({ id: "student-transfer", label: "Transfer", onClick: () => setTransferring(true) });
        }
        return (
          <>
            <BentoTile
              span={8}
              title="Current enrolment"
              data-context-status={c?.status ?? "none"}
              action={
                admin && c?.status === "pending_activation" ? (
                  <button
                    type="button"
                    className="sb-btn"
                    data-action="student-enrol"
                    disabled={busy}
                    onClick={() =>
                      ask({
                        title: "Complete enrolment",
                        eyebrow: `Students · ${s.displayName}`,
                        name: "student-enrol",
                        submitLabel: "Enrol",
                        fields: [
                          { name: "enrolmentNumber", label: "Enrolment number", hint: "Leave blank to allocate the next number" },
                          { name: "rollNumber", label: "Roll number" },
                          { name: "batchCode", label: "Batch", initial: c.batchCode ?? "" },
                          { name: "sectionCode", label: "Section", initial: c.sectionCode ?? "" },
                        ],
                        onSubmit: (v) => submit(() => api.enrol(s.id, c.version, { enrolmentNumber: v.enrolmentNumber || null, rollNumber: v.rollNumber || null, batchCode: v.batchCode || null, sectionCode: v.sectionCode || null }), "Enrolment completed"),
                      })
                    }
                  >
                    Complete enrolment
                  </button>
                ) : admin && actions.length ? (
                  <Actions disabled={busy} actions={actions} label="Enrolment actions" />
                ) : undefined
              }
            >
              {c ? (
                <Facts
                  rows={[
                    ["Status", <StatusBadge key="s" status={c.status} />],
                    ["Programme", names.programme(c.programmeId)],
                    ["Enrolment no.", c.enrolmentNumber],
                    ["Roll no.", c.rollNumber],
                    ["Type", humanize(c.enrolmentType)],
                    ["Enrolled on", c.enrolledOn ? formatDate(c.enrolledOn) : null],
                    ["Semester", c.currentTermNo ? `Semester ${c.currentTermNo}` : null],
                    ["Batch · section", [c.batchCode, c.sectionCode].filter(Boolean).join(" · ") || null],
                  ]}
                />
              ) : (
                <EmptyState title="No enrolment" compact>
                  The student has no institution context.
                </EmptyState>
              )}
            </BentoTile>
            <BentoTile span={4} title="All enrolments">
              <div className="st-lrows">
                {s.contexts.map((x) => (
                  <ListRow key={x.id} title={names.institution(x.institutionId)} meta={[humanize(x.enrolmentType), x.closedOn ? `closed ${formatDate(x.closedOn)}` : null].filter(Boolean).join(" · ")} badge={{ label: humanize(x.status), kind: badgeKind(x.status) }} />
                ))}
              </div>
            </BentoTile>
            {(transfers.data ?? []).length > 0 && (
            <BentoTile span={(history.data ?? []).length ? 6 : 12} title="Transfers">
              {(transfers.data ?? []).length === 0 ? (
                <EmptyState title="No transfers" compact>
                  Intra- and inter-institute transfers appear here.
                </EmptyState>
              ) : (
                <div className="st-lrows">
                  {(transfers.data ?? []).map((t) => (
                    <ListRow key={t.id} title={humanize(t.kind)} meta={t.effectiveOn ? `effective ${formatDate(t.effectiveOn)}` : "awaiting approval"} badge={{ label: humanize(t.status), kind: badgeKind(t.status) }} />
                  ))}
                </div>
              )}
            </BentoTile>
            )}
            {(history.data ?? []).length > 0 && (
            <BentoTile span={(transfers.data ?? []).length ? 6 : 12} title="Enrolment & assignment history">
              {(history.data ?? []).length === 0 ? (
                <EmptyState title="No changes yet" compact>
                  Enrolment, assignment and profile changes appear here.
                </EmptyState>
              ) : (
                <div className="st-lrows">
                  {[...(history.data ?? [])].reverse().map((h) => (
                    <ListRow key={`${h.occurredAt}-${h.kind}`} icon={History} title={humanize(h.kind)} meta={[formatDateTime(h.occurredAt), h.reason].filter(Boolean).join(" · ")} />
                  ))}
                </div>
              )}
            </BentoTile>
            )}
            {transferring && <TransferDialog s={s} onClose={() => setTransferring(false)} />}
            {prompt}
            {dialog}
          </>
        );
      }}
    </Loading>
  );
}

// ================================================================== documents

export function Student360Documents({ studentId }: { studentId: string }) {
  const api = useRecordApi();
  const { role } = useSession();
  const admin = role === "administrator";
  const docs = useQuery({ queryKey: rkeys.scoped("student", studentId, "documents"), queryFn: () => api.documents(studentId), enabled: !!studentId });
  const requests = useQuery({ queryKey: rkeys.scoped("student", studentId, "requests"), queryFn: () => api.studentRequests(studentId), enabled: !!studentId });
  const { run, submit, busy, dialog } = useRecordCommand();
  const { ask, prompt } = useAsk();
  const docList = docs.data ?? [];
  const reqList = requests.data ?? [];
  const raiseButton = admin ? (
    <button
              type="button"
              className="sb-btn sb-btn--ghost"
              data-raise-request
              onClick={() =>
                ask({
                  title: "Raise a request",
                  eyebrow: "Students · Documents",
                  name: "student-request",
                  submitLabel: "Raise request",
                  fields: [
                    { name: "documentType", label: "Document", type: "select", initial: REQUEST_TYPES[0], options: REQUEST_TYPES.map((t) => ({ value: t, label: humanize(t) })), wide: true },
                    { name: "purpose", label: "Purpose", wide: true, placeholder: "For example, bank account opening" },
                  ],
                  onSubmit: (v) => submit(() => api.raiseRequest(studentId, { documentType: v.documentType!, purpose: v.purpose || null }), "Request raised"),
                })
              }
            >
              Raise request
            </button>
  ) : null;
  return (
    <>
      <BentoTile
        span={12}
        title="Documents"
        data-documents=""
        action={
          admin ? (
            <span style={{ display: "inline-flex", gap: 8 }}>
            <button
              type="button"
              className="sb-btn sb-btn--ghost"
              data-add-student-document
              onClick={() =>
                ask({
                  title: "File a document",
                  eyebrow: "Students · Documents",
                  name: "student-document",
                  submitLabel: "File",
                  fields: [
                    { name: "documentType", label: "Type", type: "select", required: true, initial: "transfer_certificate", wide: true, options: ["transfer_certificate", "identity_proof", "qualifying_marksheet", "photo", "migration_certificate", "category_certificate", "medical_certificate", "other"].map((t) => ({ value: t, label: humanize(t) })) },
                    { name: "title", label: "Title", required: true, placeholder: "Transfer certificate (school)" },
                    { name: "fileRef", label: "Storage reference", placeholder: "Document store link" },
                  ],
                  onSubmit: (v) => submit(() => api.addDocument(studentId, { documentType: v.documentType!.trim(), title: v.title!.trim(), fileRef: v.fileRef || undefined }), "Document filed"),
                })
              }
            >
              <FileText strokeWidth={1.75} aria-hidden="true" />
              File document
            </button>
            {raiseButton}
            </span>
          ) : undefined
        }
      >
        {docList.length === 0 ? (
          <EmptyState icon={FileText} title="No documents" compact>
            Verified admission documents and issued certificates appear here.
          </EmptyState>
        ) : (
          <div className="st-lrows">
            {docList.map((d) => (
              <ListRow
                key={d.id}
                data-document={d.documentType}
                icon={FileText}
                tone={d.status === "verified" ? "green" : d.status === "rejected" ? "rose" : "amber"}
                title={humanize(d.documentType)}
                meta={[d.title, d.status === "verified" ? "Verified by the registrar’s office" : humanize(d.source)].filter(Boolean).join(" · ")}
                badge={{ label: humanize(d.status), kind: d.status === "verified" ? "done" : d.status === "rejected" ? "attention" : "review" }}
                {...(admin && d.status === "submitted"
                  ? rowProps(
                      [
                        { id: "student-doc-verify", label: "Verify", variant: "primary", onClick: () => void run(() => api.verifyDocument(studentId, d.id, d.version, "verified"), "Document verified") },
                        { id: "student-doc-reject", label: "Reject", onClick: () => ask({ title: "Reject document", eyebrow: "Students · Documents", name: "student-doc-reject", submitLabel: "Reject", fields: [{ name: "note", label: "Why", required: true, wide: true, type: "textarea" }], onSubmit: (v) => submit(() => api.verifyDocument(studentId, d.id, d.version, "rejected", v.note), "Document rejected") }) },
                      ],
                      busy,
                    )
                  : {})}
              />
            ))}
          </div>
        )}
      </BentoTile>
      {reqList.length > 0 && (
      <BentoTile span={12} title="Document & certificate requests">
        {reqList.length === 0 ? (
          <EmptyState title="No requests" compact>
            Requests raised by the student or staff appear here.
          </EmptyState>
        ) : (
          <div className="st-lrows">
            {reqList.map((r) => {
              const acts: RowAction[] = [];
              if (admin && r.status === "requested") acts.push({ id: "request-start", label: "Start", variant: "primary", onClick: () => void run(() => api.requestCommand(r.id, r.version, "start"), "Request in progress") });
              if (admin && (r.status === "requested" || r.status === "in_progress")) {
                acts.push({ id: "request-issue", label: "Issue", variant: r.status === "in_progress" ? "primary" : undefined, onClick: () => ask({ title: "Record issue", eyebrow: "Students · Documents", name: "request-issue", submitLabel: "Issue", fields: [{ name: "issuedReference", label: "Document reference", required: true, wide: true, placeholder: "BON/2026/0001" }], onSubmit: (v) => submit(() => api.issueRequest(r.id, r.version, v.issuedReference!.trim()), "Document issued") }) });
                acts.push({ id: "request-reject", label: "Reject", onClick: () => ask({ title: "Reject request", eyebrow: "Students · Documents", name: "request-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.requestCommand(r.id, r.version, "reject", v.reason), "Request rejected") }) });
              }
              return (
                <ListRow
                  key={r.id}
                  data-request={r.documentType}
                  icon={FileText}
                  tone="blue"
                  title={humanize(r.documentType)}
                  meta={[r.channel === "self_service" ? "By the student" : "By staff", formatDate(r.createdAt.slice(0, 10)), r.purpose, r.issuedReference ? `ref ${r.issuedReference}` : r.statusReason].filter(Boolean).join(" · ")}
                  badge={{ label: humanize(r.status), kind: badgeKind(r.status) }}
                  {...rowProps(acts, busy)}
                />
              );
            })}
          </div>
        )}
      </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

