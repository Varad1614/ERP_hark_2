/**
 * Slice **6B** web components (assessment & marks). The export names are imported by the
 * foundation route frames (docs/batch6-contracts.md §10); real screens carry `data-slice="6B"`.
 * Only @sutra/ui components — no custom styling. Marks / percentages / grade points stay the
 * decimal strings the API sends.
 *
 * - `MarksAdminTab` — Admin → Examinations → Marks (mark sheets: open, internal import,
 *   verification, final marks approval, finalisation; withheld marks; moderation; gradebook).
 * - `MalpracticeAdminTab` — Admin → Examinations → Malpractice (cases: investigation, hearing,
 *   committee decision, appeal review, pending approvals, reopening, closure, archival).
 * - `FacultyAssessmentsPanel` — Faculty `/assessments` (papers of the sections they teach,
 *   marks entry grid, submission, gradebook).
 */
import { CheckCircle2, ClipboardCheck, FileWarning, Gavel, Inbox, NotebookPen } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, Input, Select, StatTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, REASON, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { mkeys, useMarksApi } from "./hooks";
import type { Assessment, MalpracticeCase, MarkRow, MarkRowList, Moderation } from "./types";

export { MarksApiProvider } from "./hooks";

const PENALTIES = [
  { value: "warning", label: "Warning" },
  { value: "marks_deduction", label: "Marks deduction" },
  { value: "paper_cancelled", label: "Paper cancelled" },
  { value: "session_cancelled", label: "Session cancelled" },
  { value: "debarred", label: "Debarred" },
  { value: "none", label: "No penalty" },
];

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="6B">
      <EmptyState icon={ClipboardCheck} title="No institution selected">
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

// ------------------------------------------------------------------ marks grid (shared)

function MarksGrid({ paperId, staff }: { paperId: string; staff: boolean }) {
  const api = useMarksApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [showGradebook, setShowGradebook] = useState(false);
  const marks = useQuery({ queryKey: mkeys(paperId, "marks"), queryFn: () => api.marks(paperId) });
  const gradebook = useQuery({ queryKey: mkeys(paperId, "gradebook"), queryFn: () => api.gradebook(paperId), enabled: showGradebook });
  const moderations = useQuery({ queryKey: mkeys(paperId, "moderation"), queryFn: () => api.moderations(paperId), enabled: staff });
  const data: MarkRowList | undefined = marks.data;
  const sheet = data?.sheet ?? null;
  const editable = (r: MarkRow) => r.effective && (r.status === "pending" || r.status === "entered");
  const changed = (data?.items ?? []).filter((r) => draft[r.id] !== undefined && draft[r.id] !== (r.externalMarks ?? ""));
  const save = () =>
    run(async () => {
      const res = await api.enter(
        paperId,
        changed.map((r) => (draft[r.id] === "AB" ? { id: r.id, version: r.version, absent: true } : { id: r.id, version: r.version, externalMarks: draft[r.id] })),
      );
      setDraft({});
      return res;
    }, "Marks saved");
  const rowActions = (r: MarkRow): RowAction[] => {
    if (!staff || !r.effective) return [];
    const a: RowAction[] = [];
    if (r.status === "withheld") a.push({ id: "mark-release", label: "Release", onClick: () => void run(() => api.release(r.id, r.version), "Marks released") });
    else if (r.status !== "finalised")
      a.push({
        id: "mark-withhold",
        label: "Withhold",
        onClick: () => ask({ title: `Withhold marks of ${r.studentNumber}`, name: "mark-withhold", fields: [REASON], submitLabel: "Withhold", onSubmit: (v) => submit(() => api.withhold(r.id, r.version, v.reason!), "Marks withheld") }),
      });
    if (sheet)
      a.push({
        id: "malpractice-report",
        label: "Report malpractice",
        onClick: () =>
          ask({
            title: `Report malpractice — ${r.studentNumber}`,
            name: "malpractice-report",
            fields: [{ name: "allegation", label: "Allegation", required: true, wide: true }, { name: "evidence", label: "Evidence", wide: true }],
            submitLabel: "Report",
            onSubmit: (v) => submit(() => api.reportCase(sheet.sessionId, { registrationId: r.registrationId, paperId: r.paperId, allegation: v.allegation!, evidence: v.evidence || undefined }), (c) => `Case ${c.caseNumber} reported`),
          }),
      });
    return a;
  };
  const sheetActions: RowAction[] = [];
  if (sheet && sheet.status === "in_evaluation") sheetActions.push({ id: "sheet-submit", label: "Submit marks", variant: "primary", onClick: () => void run(() => api.sheetCommand(sheet.id, sheet.version, "submit"), "Marks submitted") });
  if (staff && sheet) {
    if (["planned", "assigned", "in_evaluation", "evaluated", "validated"].includes(sheet.status))
      sheetActions.push({ id: "sheet-import", label: "Import internal", onClick: () => void run(() => api.importInternal(sheet.id), (s) => `Internal marks imported: ${s.processed} updated, ${s.missing.length} missing`) });
    if (sheet.status === "evaluated") sheetActions.push({ id: "sheet-verify", label: "Verify", variant: "primary", onClick: () => void run(() => api.sheetCommand(sheet.id, sheet.version, "verify"), "Marks verified") });
    if (sheet.status === "validated" && !sheet.awaitingDecision) {
      sheetActions.push({ id: "sheet-approval", label: "Request final approval", variant: "primary", onClick: () => void run(() => api.sheetCommand(sheet.id, sheet.version, "request-approval"), "Final marks approval requested") });
      sheetActions.push({
        id: "moderation-request",
        label: "Moderate",
        onClick: () =>
          ask({
            title: "Request moderation",
            name: "moderation-request",
            fields: [
              { name: "kind", label: "Kind", type: "select", required: true, initial: "grace", options: [{ value: "grace", label: "Grace marks" }, { value: "scaling", label: "Scaling (%)" }] },
              { name: "value", label: "Value", required: true, placeholder: "3.00" },
              REASON,
            ],
            submitLabel: "Request",
            onSubmit: (v) => submit(() => api.requestModeration(paperId, v.kind!, v.value!, v.reason!), "Moderation requested"),
          }),
      });
    }
    if (sheet.awaitingDecision) {
      sheetActions.push({ id: "sheet-approve", label: "Approve final marks", variant: "primary", onClick: () => void run(() => api.decideSheet(sheet.id, sheet.version, true), "Final marks approved") });
      sheetActions.push({ id: "sheet-reject", label: "Reject", onClick: () => ask({ title: "Reject final marks", name: "sheet-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideSheet(sheet.id, sheet.version, false, v.reason), "Approval rejected") }) });
    }
    if (sheet.status === "approved") sheetActions.push({ id: "sheet-finalise", label: "Finalise", variant: "primary", onClick: () => void run(() => api.sheetCommand(sheet.id, sheet.version, "finalise"), "Final approved marks established") });
  }
  const modActions = (m: Moderation): RowAction[] => {
    const a: RowAction[] = [];
    if (m.status === "requested") a.push({ id: "moderation-validate", label: "Validate", onClick: () => void run(() => api.moderationCommand(m.id, m.version, "validate"), "Moderation validated") });
    if (m.status === "approved") a.push({ id: "moderation-apply", label: "Apply", variant: "primary", onClick: () => void run(() => api.moderationCommand(m.id, m.version, "apply"), "Moderation applied") });
    if (["requested", "validated", "approved"].includes(m.status)) a.push({ id: "moderation-withdraw", label: "Withdraw", onClick: () => void run(() => api.moderationCommand(m.id, m.version, "withdraw"), "Moderation withdrawn") });
    return a;
  };
  return (
    <>
      <BentoTile
        span={12}
        title={data ? `Marks · ${data.paperCode}` : "Marks"}
        data-slice="6B"
        action={
          <>
            {sheet ? <StatusBadge status={sheet.status} /> : <Badge kind="info">No sheet yet</Badge>}
            <Button size="sm" variant="ghost" onClick={() => setShowGradebook((s) => !s)} data-action="gradebook-toggle">
              {showGradebook ? "Hide gradebook" : "Gradebook"}
            </Button>
          </>
        }
      >
        <Actions actions={sheetActions} disabled={busy} />
        <DataTable<MarkRow>
          caption="Candidates and marks"
          rows={data?.items ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={NotebookPen} title="No candidates">Registrations for this paper appear here.</EmptyState>}
          columns={[
            { key: "student", header: "Candidate", render: (r) => `${r.displayName} · ${r.studentNumber}` },
            { key: "attempt", header: "Attempt", render: (r) => statusLabel(r.attemptKind), muted: true },
            { key: "internal", header: "Internal", render: (r) => (r.internalMarks ? `${r.internalMarks} / ${r.internalMax ?? "—"}` : "—") },
            {
              key: "external",
              header: `External (/${data?.items[0]?.externalMax ?? "100.00"})`,
              render: (r) =>
                editable(r) ? (
                  <Input
                    label={`External marks of ${r.studentNumber}`}
                    hideLabel
                    value={draft[r.id] ?? (r.absent ? "AB" : (r.externalMarks ?? ""))}
                    placeholder="e.g. 61.50 or AB"
                    inputMode="decimal"
                    data-mark-input={r.studentNumber}
                    onChange={(e) => setDraft((d) => ({ ...d, [r.id]: e.target.value.trim() }))}
                  />
                ) : r.absent ? (
                  "Absent"
                ) : (
                  (r.externalMarks ?? "—")
                ),
            },
            { key: "grace", header: "Grace", render: (r) => r.graceMarks, muted: true },
            { key: "preview", header: "Preview", render: (r) => (r.preview ? `${r.preview.percent ?? "—"} % · ${r.preview.grade ?? "—"}` : "—") },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions actions={rowActions(r)} disabled={busy} /> },
          ]}
        />
        {changed.length > 0 && (
          <Button size="sm" onClick={() => void save()} disabled={busy} data-action="marks-save">
            Save {changed.length} row(s)
          </Button>
        )}
      </BentoTile>
      {showGradebook && gradebook.data && (
        <BentoTile span={12} title={`Gradebook · ${gradebook.data.paperCode}`} data-gradebook>
          <p>
            {gradebook.data.graded} graded · {gradebook.data.passed} passed · {gradebook.data.failed} failed · {gradebook.data.absent} absent · average {gradebook.data.averagePercent ?? "—"} %
          </p>
          <DataTable
            caption="Gradebook"
            rows={gradebook.data.lines}
            rowKey={(l) => l.paperMarkId}
            columns={[
              { key: "student", header: "Candidate", render: (l) => `${l.displayName} · ${l.studentNumber}` },
              { key: "percent", header: "%", render: (l) => l.percent ?? "—" },
              { key: "grade", header: "Grade", render: (l) => l.grade ?? "—" },
              { key: "gp", header: "Grade point", render: (l) => l.gradePoint ?? "—" },
              { key: "passed", header: "Result", render: (l) => (l.percent ? <Badge kind={l.passed ? "done" : "attention"}>{l.passed ? "Pass" : "Fail"}</Badge> : "—") },
            ]}
          />
        </BentoTile>
      )}
      {staff && (moderations.data?.length ?? 0) > 0 && (
        <BentoTile span={12} title="Moderation">
          <DataTable<Moderation>
            caption="Moderations"
            rows={moderations.data ?? []}
            rowKey={(m) => m.id}
            columns={[
              { key: "kind", header: "Kind", render: (m) => `${statusLabel(m.kind)} ${m.value}` },
              { key: "reason", header: "Reason", render: (m) => m.reason, muted: true },
              { key: "applied", header: "Scripts", render: (m) => String(m.appliedCount) },
              { key: "status", header: "Status", render: (m) => <StatusBadge status={m.status} /> },
              { key: "actions", header: "", render: (m) => <Actions actions={modActions(m)} disabled={busy} /> },
            ]}
          />
        </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

function PaperTable({ rows, onOpen, selected, staff }: { rows: Assessment[]; onOpen: (a: Assessment) => void; selected: string | null; staff: boolean }) {
  const api = useMarksApi();
  const { run, busy, dialog } = useCommand();
  return (
    <>
      <DataTable<Assessment>
        caption="Papers"
        rows={rows}
        rowKey={(a) => a.paperId}
        empty={<EmptyState icon={NotebookPen} title="No papers yet">Papers of examination sessions appear here once scheduled.</EmptyState>}
        columns={[
          { key: "paper", header: "Paper", render: (a) => `${a.paperCode} · ${a.title}` },
          { key: "session", header: "Session", render: (a) => a.sessionCode, muted: true },
          { key: "sections", header: "Sections", render: (a) => (a.sections.length ? a.sections.join(", ") : "—"), muted: true },
          { key: "progress", header: "Entered", render: (a) => `${a.entered} / ${a.candidates}` },
          { key: "paperStatus", header: "Paper status", render: (a) => <StatusBadge status={a.paperStatus} /> },
          { key: "sheet", header: "Mark sheet", render: (a) => (a.sheetStatus ? <StatusBadge status={a.sheetStatus} /> : "—") },
          {
            key: "actions",
            header: "",
            render: (a) => (
              <Actions
                disabled={busy}
                actions={[
                  ...(staff && !a.sheetId && a.acceptsMarks ? [{ id: "sheet-open", label: "Open sheet", onClick: () => void run(() => api.openSheet(a.paperId), "Mark sheet opened") }] : []),
                  { id: "paper-open", label: selected === a.paperId ? "Selected" : "Marks", variant: selected === a.paperId ? ("primary" as const) : ("ghost" as const), onClick: () => onOpen(a) },
                ]}
              />
            ),
          },
        ]}
      />
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Admin → Marks

export function MarksAdminTab({ institution }: { institution: Institution | null }) {
  const api = useMarksApi();
  const [selected, setSelected] = useState<string | null>(null);
  const inst = institution?.id ?? "-";
  const papers = useQuery({ queryKey: mkeys(inst, "papers"), queryFn: () => api.institutionPapers(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="Marks & moderation" />;
  return (
    <>
      <BentoTile span={12} title="Mark sheets" data-slice="6B">
        <PaperTable rows={papers.data ?? []} selected={selected} staff onOpen={(a) => setSelected(a.paperId)} />
      </BentoTile>
      {selected && <MarksGrid paperId={selected} staff />}
    </>
  );
}

// ------------------------------------------------------------------ Faculty Assessments

/** Candidates still without marks on the papers that accept marks (faculty "To review"). */
export function toReviewCount(list: readonly Assessment[]): number {
  return list.filter((a) => a.acceptsMarks).reduce((n, a) => n + Math.max(0, a.candidates - a.entered), 0);
}

/** Faculty Assessments: papers of the sections they teach — marks entry and gradebook. */
export function FacultyAssessmentsPanel() {
  const api = useMarksApi();
  const [selected, setSelected] = useState<string | null>(null);
  const mine = useQuery({ queryKey: mkeys("me", "assessments"), queryFn: () => api.myAssessments() });
  const list = mine.data ?? [];
  const open = list.filter((a) => a.acceptsMarks);
  const toReview = toReviewCount(list);
  const done = list.filter((a) => a.sheetStatus && ["evaluated", "verified", "validated", "approved", "finalised"].includes(a.sheetStatus)).length;
  const loading = !mine.data;
  return (
    <>
      <StatTile span={4} icon={Inbox} tone="violet" label="To review" value={loading ? "…" : String(toReview)} hint={open[0] ? `${open.length} paper${open.length === 1 ? "" : "s"} open for marks` : "No marks pending"} data-kpi="to-review" />
      <StatTile span={4} icon={CheckCircle2} tone="green" label="Submitted" value={loading ? "…" : String(done)} hint="Mark sheets sent to the exam cell" data-kpi="submitted" />
      <StatTile span={4} icon={NotebookPen} tone="orange" label="Papers" value={loading ? "…" : String(list.length)} hint={list[0] ? `${list[0].sessionCode} · ${list[0].paperCode}` : "Of the sections you teach"} data-kpi="papers" />
      <BentoTile span={12} title="My assessments" data-slice="6B">
        <PaperTable rows={mine.data ?? []} selected={selected} staff={false} onOpen={(a) => setSelected(a.paperId)} />
      </BentoTile>
      {selected && <MarksGrid paperId={selected} staff={false} />}
    </>
  );
}

// ------------------------------------------------------------------ Admin → Malpractice

export function MalpracticeAdminTab({ institution }: { institution: Institution | null }) {
  const api = useMarksApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const papers = useQuery({ queryKey: mkeys(inst, "papers"), queryFn: () => api.institutionPapers(inst), enabled: !!institution });
  const sessions = Array.from(new Map((papers.data ?? []).map((p) => [p.sessionId, p.sessionCode])).entries());
  const [session, setSession] = useState<string>("");
  const current = session || sessions[0]?.[0] || "";
  const cases = useQuery({ queryKey: mkeys(current || "-", "cases"), queryFn: () => api.cases(current), enabled: !!current });
  if (!institution) return <NoInstitutionTile title="Malpractice" />;
  const reasonCmd = (c: MalpracticeCase, cmd: "appeal" | "reopen", label: string): RowAction => ({
    id: `case-${cmd}`,
    label,
    onClick: () => ask({ title: `${label} ${c.caseNumber}`, name: `case-${cmd}`, fields: [REASON], submitLabel: label, onSubmit: (v) => submit(() => api.caseCommand(c.id, c.version, cmd, v.reason), `${c.caseNumber}: ${label.toLowerCase()}`) }),
  });
  const actions = (c: MalpracticeCase): RowAction[] => {
    const a: RowAction[] = [];
    if (c.pending) {
      a.push({ id: "case-approve", label: "Approve decision", variant: "primary", onClick: () => void run(() => api.approveCase(c.id, c.version, true), "Decision approved") });
      a.push({ id: "case-reject", label: "Reject", onClick: () => ask({ title: `Reject the pending decision of ${c.caseNumber}`, name: "case-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.approveCase(c.id, c.version, false, v.reason), "Proposal rejected") }) });
      return a;
    }
    if (c.status === "reported") a.push({ id: "case-investigate", label: "Investigate", onClick: () => void run(() => api.caseCommand(c.id, c.version, "investigate"), "Investigation opened") });
    if (c.status === "under_investigation") a.push({ id: "case-hearing", label: "Hearing", onClick: () => void run(() => api.caseCommand(c.id, c.version, "hearing"), "Hearing scheduled") });
    if (c.status === "under_investigation" || c.status === "hearing")
      a.push({
        id: "case-decide",
        label: "Decide",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Committee decision — ${c.caseNumber}`,
            name: "case-decide",
            fields: [{ name: "penalty", label: "Penalty", type: "select", required: true, options: PENALTIES, initial: "warning" }, { name: "marksDeduction", label: "Marks deducted (for a deduction)", placeholder: "5.00" }, REASON],
            submitLabel: "Record decision",
            onSubmit: (v) => submit(() => api.decideCase(c.id, c.version, v.penalty!, v.reason!, v.marksDeduction || undefined), "Decision recorded"),
          }),
      });
    if (c.status === "decided") a.push(reasonCmd(c, "appeal", "Appeal"));
    if (c.status === "appealed" || c.status === "under_review")
      a.push({
        id: "case-review",
        label: "Review",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Appeal review — ${c.caseNumber}`,
            name: "case-review",
            fields: [
              { name: "outcome", label: "Outcome", type: "select", required: true, initial: "uphold", options: [{ value: "uphold", label: "Uphold" }, { value: "modify", label: "Modify penalty" }, { value: "overturn", label: "Overturn" }] },
              { name: "penalty", label: "New penalty (modify)", type: "select", options: [{ value: "", label: "—" }, ...PENALTIES] },
              REASON,
            ],
            submitLabel: "Record review",
            onSubmit: (v) => submit(() => api.reviewCase(c.id, c.version, v.outcome!, v.reason!, v.penalty || undefined), "Review recorded"),
          }),
      });
    if (["decided", "upheld", "modified", "overturned"].includes(c.status)) a.push({ id: "case-close", label: "Close", onClick: () => void run(() => api.caseCommand(c.id, c.version, "close"), "Case closed") });
    if (["decided", "upheld", "modified", "overturned", "closed"].includes(c.status) && !c.mergedIntoId) a.push(reasonCmd(c, "reopen", "Reopen"));
    if (c.status === "closed") a.push({ id: "case-archive", label: "Archive", onClick: () => void run(() => api.caseCommand(c.id, c.version, "archive"), "Case archived") });
    return a;
  };
  return (
    <>
      <BentoTile
        span={12}
        title="Malpractice cases"
        data-slice="6B"
        action={
          sessions.length > 0 ? (
            <Select label="Examination session" value={current} options={sessions.map(([id, code]) => ({ value: id, label: code }))} onChange={(e) => setSession(e.target.value)} />
          ) : undefined
        }
      >
        <p>Cases are reported from a candidate's row in Marks. Decisions and review outcomes follow the governed approval (auto-approved in this release).</p>
        <DataTable<MalpracticeCase>
          caption="Malpractice cases"
          rows={cases.data ?? []}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={FileWarning} title="No malpractice cases">Suspected malpractice reported for this session appears here.</EmptyState>}
          columns={[
            { key: "number", header: "Case", render: (c) => c.caseNumber },
            { key: "allegation", header: "Allegation", render: (c) => c.allegation, muted: true },
            { key: "penalty", header: "Penalty", render: (c) => (c.penalty ? `${statusLabel(c.penalty)}${c.marksDeduction ? ` (${c.marksDeduction})` : ""}` : "—") },
            { key: "status", header: "Status", render: (c) => (c.pending ? <Badge kind="review">Awaiting approval</Badge> : <StatusBadge status={c.status} />) },
            { key: "actions", header: "", render: (c) => <Actions actions={actions(c)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Decision trail">
        {(cases.data ?? []).some((c) => c.decisions.length > 0) ? (
          <ul>
            {(cases.data ?? []).flatMap((c) =>
              c.decisions.map((d) => (
                <li key={d.id}>
                  <Gavel size={14} /> {c.caseNumber}: {statusLabel(d.kind)} — {statusLabel(d.outcome)}
                  {d.penalty ? ` · ${statusLabel(d.penalty)}` : ""}
                  {d.reason ? ` · ${d.reason}` : ""}
                </li>
              )),
            )}
          </ul>
        ) : (
          <EmptyState icon={Gavel} title="No decisions yet" />
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
