/**
 * Slice **6C** web components (results, publication, grade cards, revaluation, supplementary
 * eligibility, completion and closure). Export names are imported by the foundation frames
 * (`routes/admin/examinations.tsx`, `routes/student/results.tsx`); real screens carry
 * `data-slice="6C"`. Marks, SGPA and CGPA stay decimal strings from the API (never parsed).
 *
 * - `ResultsAdminTab` — Admin → Examinations → Results & publication.
 * - `RevaluationAdminTab` — Admin → Examinations → Revaluation (reviews, supplementary
 *   eligibility, completion, session closure).
 * - `StudentResultsPanel` — student `/results` (span 8): results with SGPA / CGPA, grade cards,
 *   revaluation / rechecking requests and their fee status.
 */
import { Award, FileCheck2, GraduationCap, Layers, ListChecks, RotateCcw, TrendingUp } from "lucide-react";
import { AreaHeader, Badge, BentoTile, Button, DataTable, EmptyState, KpiTile, ListRow, NameCell, RowActions, formatDate, formatINR, formatNumber, plural, statusKind, type BadgeKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { NoInstitution, REASON, type RowAction } from "../../institution/forms";
import { useStudentOptions } from "../setup/pickers";
import { ExamActions as Actions, SessionSelect, splitActions, useExamAsk as useAsk } from "../ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { rkeys, useResultsApi } from "./hooks";
import type { Publication, ResultBatch, Revaluation, TermResult } from "./types";

export { ResultsApiProvider } from "./hooks";

const dash = (v: string | null | undefined) => v ?? "—";

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="6C">
      <NoInstitution />
    </BentoTile>
  );
}

/** Session picker shared by both admin tabs. */
function useSession(institution: Institution) {
  const api = useResultsApi();
  const sessions = useQuery({ queryKey: rkeys.scoped(institution.id, "sessions"), queryFn: () => api.sessions(institution.id) });
  const [picked, setPicked] = useState<string>("");
  const list = sessions.data ?? [];
  const sessionId = picked || list[0]?.id || "";
  const picker = <SessionSelect value={sessionId} onChange={setPicked} field="session" options={list.map((s) => ({ value: s.id, label: `${s.name} · ${statusLabel(s.status)}` }))} />;
  return { sessionId, picker, empty: sessions.isSuccess && list.length === 0 };
}

function batchActions(b: ResultBatch): Array<"compute" | "validate" | "finalise"> {
  switch (b.status) {
    case "pending":
      return ["compute"];
    case "computed":
      return ["compute", "validate"];
    case "approved":
      return ["finalise"];
    default:
      return [];
  }
}

function resultActions(r: TermResult): Array<"withhold" | "release" | "recompute" | "withdraw" | "restore" | "correct" | "card"> {
  switch (r.status) {
    case "computed":
    case "validated":
      return ["recompute", "withhold", "correct"];
    case "approved":
      return ["withhold", "correct"];
    case "withheld":
      return ["release"];
    case "published":
      return ["card", "correct", "withdraw"];
    case "withdrawn":
      return ["restore"];
    default:
      return [];
  }
}

function publicationActions(p: Publication): Array<"authorise" | "approve" | "publish" | "delay" | "resume" | "retry" | "cancel"> {
  switch (p.status) {
    case "planned":
    case "scheduled":
      return p.processInstanceId ? ["approve", "cancel"] : ["authorise", "delay", "cancel"];
    case "authorised":
    case "ready":
      return ["publish", "delay", "cancel"];
    case "delayed":
      return ["resume", "cancel"];
    case "failed":
      return ["retry", "cancel"];
    default:
      return [];
  }
}

/** Admin → Examinations → Results & publication. */
export function ResultsAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <ResultsAdmin institution={institution} />;
}

function ResultsAdmin({ institution }: { institution: Institution }) {
  const api = useResultsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { sessionId, picker, empty } = useSession(institution);
  const enabled = sessionId !== "";
  const batches = useQuery({ queryKey: rkeys.scoped(sessionId, "batches"), queryFn: () => api.batches(sessionId), enabled });
  const results = useQuery({ queryKey: rkeys.scoped(sessionId, "results"), queryFn: () => api.results(sessionId), enabled });
  const pubs = useQuery({ queryKey: rkeys.scoped(sessionId, "publications"), queryFn: () => api.publications(sessionId), enabled });
  const batch = batches.data?.[0];

  const onResult = (r: TermResult, a: ReturnType<typeof resultActions>[number]) => {
    if (a === "card") return run(() => api.issueCard(r.id), (c) => `Grade card ${c.number} issued`);
    if (a === "correct")
      return ask({
        title: `Correct marks · ${r.studentNumber}`,
        name: "result-correct",
        submitLabel: "Request correction",
        fields: [
          { name: "paperId", label: "Paper", type: "select", required: true, options: r.lines.map((l) => ({ value: l.paperId, label: `${l.paperCode} — ${l.title}` })), initial: r.lines[0]?.paperId },
          { name: "externalMarks", label: "Corrected external marks", required: true, placeholder: "64.00" },
          REASON,
        ],
        onSubmit: (v) => submit(() => api.correct(r.id, r.version, { paperId: v.paperId!, externalMarks: v.externalMarks, reason: v.reason! }), "Correction recorded"),
      });
    if (a === "withhold" || a === "withdraw") {
      const cmd = a;
      return ask({ title: `${statusLabel(cmd)} · ${r.studentNumber}`, name: `result-${cmd}`, submitLabel: statusLabel(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.resultCommand(r.id, r.version, cmd, v.reason), `${r.studentNumber}: ${statusLabel(cmd)}`) });
    }
    const cmd = a;
    return run(() => api.resultCommand(r.id, r.version, cmd), `${r.studentNumber}: ${statusLabel(cmd)}`);
  };

  const onPublication = (p: Publication, a: ReturnType<typeof publicationActions>[number]) => {
    if (a === "approve") return run(() => api.decidePublication(p.id, p.version, "approve"), "Publication authorised");
    if (a === "delay") return ask({ title: "Delay publication", name: "publication-delay", submitLabel: "Delay", fields: [REASON], onSubmit: (v) => submit(() => api.delayPublication(p.id, p.version, v.reason!), "Publication delayed") });
    if (a === "cancel") return ask({ title: "Cancel publication", name: "publication-cancel", submitLabel: "Cancel publication", fields: [REASON], onSubmit: (v) => submit(() => api.cancelPublication(p.id, p.version, v.reason!), "Publication cancelled") });
    const cmd = a;
    return run(() => api.publicationCommand(p.id, p.version, cmd), `Publication: ${statusLabel(cmd)}`);
  };

  const batchActs = (b: ResultBatch): RowAction[] => {
    const acts: RowAction[] = batchActions(b).map((c) => ({ id: `batch-${c}`, label: statusLabel(c), variant: c === "validate" ? "primary" : "ghost", onClick: () => void run(() => api.batchCommand(b.id, b.version, c), `Batch: ${statusLabel(c)}`) }));
    if (b.status === "validated" && b.processInstanceId) {
      acts.push({ id: "batch-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideBatch(b.id, b.version, "approve"), "Results approved") });
      acts.push({ id: "batch-reject", label: "Reject", onClick: () => ask({ title: "Reject results", name: "batch-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideBatch(b.id, b.version, "reject", v.reason), "Results sent back") }) });
    }
    return acts;
  };

  return (
    <>
      <BentoTile span={12} title="Result computation" data-slice="6C" data-result-batch action={
        enabled && !batch ? (
          <Button data-action="new-batch" disabled={busy} onClick={() => run(() => api.createBatch(sessionId), "Result batch opened")}>
            Open batch
          </Button>
        ) : undefined
      }>
        {picker}
        {empty ? (
          <EmptyState icon={ListChecks} title="No examination sessions">Sessions are set up under Sessions & policy.</EmptyState>
        ) : batch ? (
          <ListRow
            data-batch={batch.status}
            icon={ListChecks}
            tone="violet"
            title="Result batch"
            meta={[plural(batch.runCount, "run"), `${formatNumber(batch.computedCount)} computed`, `${formatNumber(batch.withheldCount)} withheld`, batch.statusReason].filter(Boolean).join(" · ")}
            badge={{ label: statusLabel(batch.status), kind: statusKind(batch.status) }}
            {...splitActions(batchActs(batch), busy)}
          />
        ) : (
          <EmptyState icon={ListChecks} title="No result batch yet">Open a batch, then compute results from final marks.</EmptyState>
        )}
      </BentoTile>
      {((results.data?.length ?? 0) > 0 || !!batch) && (
      <BentoTile span={12} title="Results" data-results>
        <DataTable caption="Term results" rows={results.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={GraduationCap} title="No results">Compute the batch to produce results.</EmptyState>}
          columns={[
            { key: "student", header: "Candidate", render: (r) => <NameCell name={r.displayName} sub={r.studentNumber} /> },
            { key: "rev", header: "Revision", render: (r) => formatNumber(r.revision), muted: true },
            { key: "credits", header: "Credits", render: (r) => `${formatNumber(r.creditsEarned)} / ${formatNumber(r.creditsAttempted)}` },
            { key: "sgpa", header: "SGPA", render: (r) => dash(r.sgpa) },
            { key: "cgpa", header: "CGPA", render: (r) => dash(r.cgpa) },
            { key: "outcome", header: "Outcome", render: (r) => <StatusBadge status={r.outcome} /> },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "reason", header: "Note", render: (r) => dash(r.statusReason), muted: true },
            { key: "actions", header: "", render: (r) => <Actions disabled={busy} actions={resultActions(r).map((a) => ({ id: `result-${a}`, label: a === "card" ? "Grade card" : statusLabel(a), onClick: () => void onResult(r, a) }))} /> },
          ]}
        />
      </BentoTile>
      )}
      {((pubs.data?.length ?? 0) > 0 || (!!batch && !["pending", "computed", "validated"].includes(batch.status))) && (
      <BentoTile span={12} title="Publication" data-publications action={
        enabled ? (
          <Button data-action="new-publication" disabled={busy} onClick={() => run(() => api.createPublication(sessionId), "Publication planned")}>
            Plan publication
          </Button>
        ) : undefined
      }>
        <DataTable caption="Publications" rows={pubs.data ?? []} rowKey={(p) => p.id}
          empty={<EmptyState icon={FileCheck2} title="No publications">Plan one once the results are approved.</EmptyState>}
          columns={[
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "published", header: "Published", render: (p) => formatNumber(p.publishedCount) },
            { key: "skipped", header: "Withheld", render: (p) => formatNumber(p.skippedCount) },
            { key: "note", header: "Note", render: (p) => dash(p.lastError ?? p.statusReason), muted: true },
            { key: "actions", header: "", render: (p) => <Actions disabled={busy} actions={publicationActions(p).map((a) => ({ id: `publication-${a}`, label: statusLabel(a), variant: a === "publish" ? "primary" : "ghost", onClick: () => void onPublication(p, a) }))} /> },
          ]}
        />
      </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

function reviewActions(r: Revaluation): Array<"verify" | "approve" | "reject" | "start-review" | "complete" | "verify-outcome" | "approve-marks" | "withdraw"> {
  switch (r.status) {
    case "eligibility_verified":
    case "fee_pending":
      return ["verify", "withdraw"];
    case "fee_verified":
      return ["approve", "reject"];
    case "approved":
      return ["start-review", "withdraw"];
    case "review_in_progress":
      return ["complete"];
    case "review_completed":
      return ["verify-outcome"];
    case "outcome_verified":
      return ["approve-marks"];
    default:
      return [];
  }
}

/** Admin → Examinations → Revaluation. */
export function RevaluationAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <RevaluationAdmin institution={institution} />;
}

function RevaluationAdmin({ institution }: { institution: Institution }) {
  const api = useResultsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { sessionId, picker } = useSession(institution);
  const enabled = sessionId !== "";
  const reviews = useQuery({ queryKey: rkeys.scoped(sessionId, "reviews"), queryFn: () => api.reviews(sessionId), enabled });
  const elig = useQuery({ queryKey: rkeys.scoped(sessionId, "eligibility"), queryFn: () => api.eligibility(sessionId), enabled });
  const closure = useQuery({ queryKey: rkeys.scoped(sessionId, "closure"), queryFn: () => api.closure(sessionId), enabled });
  const completions = useQuery({ queryKey: rkeys.scoped(institution.id, "completions"), queryFn: () => api.completions(institution.id) });
  const studentOptions = useStudentOptions(institution.id);

  const onReview = (r: Revaluation, a: ReturnType<typeof reviewActions>[number]) => {
    if (a === "approve") return run(() => api.decideReview(r.id, r.version, "approve"), `${r.number} approved`);
    if (a === "reject") return ask({ title: `Reject ${r.number}`, name: "review-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideReview(r.id, r.version, "reject", v.reason), `${r.number} rejected`) });
    if (a === "withdraw") return run(() => api.withdrawReview(r.id, r.version), `${r.number} withdrawn`);
    if (a === "complete")
      return ask({
        title: `Review outcome · ${r.number}`,
        name: "review-complete",
        submitLabel: "Record outcome",
        fields: [
          { name: "externalMarks", label: "Revised external marks", required: true, placeholder: r.outcome?.fromMarks ?? "" },
          { name: "notes", label: "Notes", wide: true },
        ],
        onSubmit: (v) => submit(() => api.completeReview(r.id, r.version, v.externalMarks!, v.notes), `${r.number}: outcome recorded`),
      });
    const step = a;
    return run(() => api.reviewStep(r.id, r.version, step), `${r.number}: ${statusLabel(step)}`);
  };

  const c = closure.data;
  const closureActs: RowAction[] = !c || c.status === "reopened"
    ? [{ id: "closure-initiate", label: "Start closure", onClick: () => void run(() => api.closureCommand(sessionId, "initiate"), "Closure started") }]
    : c.status === "closing" && !c.processInstanceId
      ? [{ id: "closure-close", label: "Close session", variant: "primary", onClick: () => void run(() => api.closureCommand(sessionId, "close", c.version), "Session closed") }]
      : c.status === "closed"
        ? [{ id: "closure-archive", label: "Archive", onClick: () => void run(() => api.closureCommand(sessionId, "archive", c.version), "Session archived") }]
        : [];

  const eligRows = elig.data ?? [];
  const compRows = completions.data ?? [];
  const determine: RowAction = { id: "determine-eligibility", label: "Determine supplementary eligibility", onClick: () => void run(() => api.determineEligibility(sessionId), (s) => `${s.eligible} eligible, ${s.exhausted} exhausted`) };
  const evaluate: RowAction = {
    id: "evaluate-completion",
    label: "Evaluate completion",
    onClick: () => ask({ title: "Evaluate completion", name: "completion", submitLabel: "Evaluate", fields: [{ name: "studentId", label: "Student", type: "select", required: true, wide: true, options: studentOptions }], onSubmit: (v) => submit(() => api.evaluateCompletion(institution.id, v.studentId!.trim()), (r) => `${r.studentNumber}: ${statusLabel(r.status)}`) }),
  };
  // Empty sub-areas fold into the main tile's kebab instead of stacking empty tiles.
  const folded: RowAction[] = [...(enabled && eligRows.length === 0 ? [determine] : []), ...(compRows.length === 0 ? [evaluate] : []), ...(enabled && !c ? closureActs : [])];

  return (
    <>
      <BentoTile span={12} title="Revaluation & rechecking" data-slice="6C" data-reviews action={<Actions actions={folded} disabled={busy} menuOnly label="Revaluation actions" />}>
        {picker}
        <DataTable caption="Review requests" rows={reviews.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={RotateCcw} title="No review requests">Students request reviews of published results.</EmptyState>}
          columns={[
            { key: "number", header: "Request", render: (r) => (
                <span>
                  <b>{statusLabel(r.kind)}</b> <small style={{ color: "var(--sb-mute-2)" }}>{r.number}</small>
                </span>
              ) },
            { key: "student", header: "Candidate", render: (r) => <NameCell name={r.displayName} sub={r.studentNumber} /> },
            { key: "paper", header: "Paper", render: (r) => r.paperCode },
            { key: "fee", header: "Fee", render: (r) => `${formatINR(r.feeAmount)} · ${r.feeSettled ? "paid" : "due"}` },
            { key: "outcome", header: "Marks", render: (r) => (r.outcome ? `${dash(r.outcome.fromMarks)} → ${dash(r.outcome.toMarks)}` : "—") },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions disabled={busy} actions={reviewActions(r).map((a) => ({ id: `review-${a}`, label: statusLabel(a), onClick: () => void onReview(r, a) }))} /> },
          ]}
        />
      </BentoTile>
      {eligRows.length > 0 && (
      <BentoTile span={c ? 7 : 12} title="Supplementary eligibility" data-eligibility action={enabled ? <Actions actions={[{ ...determine, label: "Determine" }]} disabled={busy} /> : undefined}>
        <DataTable caption="Subsequent attempts" rows={elig.data ?? []} rowKey={(e) => e.id}
          empty={<EmptyState icon={ListChecks} title="No eligibility yet">Determine it from published failed papers.</EmptyState>}
          columns={[
            { key: "student", header: "Candidate", render: (e) => <NameCell name={e.displayName} sub={e.studentNumber} /> },
            { key: "paper", header: "Paper", render: (e) => e.paperCode },
            { key: "attempts", header: "Attempts", render: (e) => `${formatNumber(e.attemptsUsed)} / ${formatNumber(e.maxAttempts)}` },
            { key: "best", header: "Best", render: (e) => (e.bestPercent ? `${e.bestPercent}%${e.bestGrade ? ` · ${e.bestGrade}` : ""}` : "—") },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
          ]}
        />
      </BentoTile>
      )}
      {c && (
      <BentoTile span={eligRows.length > 0 ? 5 : 12} title="Session closure" data-closure action={<Actions actions={enabled ? closureActs : []} disabled={busy} />}>
        {c ? (
          <ListRow
            data-closure-status={c.status}
            icon={Award}
            tone="green"
            title={statusLabel(c.status)}
            meta={[c.checks.blockers && c.checks.blockers.length > 0 ? `Blockers: ${c.checks.blockers.join(", ")}` : "No blockers", `${formatNumber(c.completedRegistrations)} completed`].join(" · ")}
            badge={{ label: statusLabel(c.status), kind: statusKind(c.status) }}
          />
        ) : (
          <EmptyState icon={Award} title="Not closing" compact>Close the session once results are published and reviews are done.</EmptyState>
        )}
      </BentoTile>
      )}
      {compRows.length > 0 && (
      <BentoTile span={12} title="Completion evaluations" data-completions action={<Actions actions={[{ ...evaluate, label: "Evaluate student" }]} disabled={busy} />}>
        <DataTable caption="Completion" rows={completions.data ?? []} rowKey={(x) => x.id}
          empty={<EmptyState icon={GraduationCap} title="No evaluations">Evaluate a student's credits and CGPA against the curriculum.</EmptyState>}
          columns={[
            { key: "student", header: "Student", render: (x) => <NameCell name={x.displayName} sub={x.studentNumber} /> },
            { key: "credits", header: "Credits", render: (x) => `${formatNumber(x.creditsEarned)} / ${formatNumber(x.creditsRequired)}` },
            { key: "components", header: "Components", render: (x) => `${formatNumber(x.componentsPassed)} / ${formatNumber(x.componentsRequired)}` },
            { key: "cgpa", header: "CGPA", render: (x) => dash(x.cgpa) },
            { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
          ]}
        />
      </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

/** Student Results: published results, grade cards and review requests (span 8). */
/** The student's current published result: latest publication first, highest revision. */
export function latestResult(list: readonly TermResult[]): TermResult | undefined {
  return [...list].filter((r) => r.status === "published").sort((a, b) => (b.publishedAt ?? "").localeCompare(a.publishedAt ?? "") || b.revision - a.revision)[0];
}

const MON = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** Tab label of a result: the publication month ("Nov 2026"), revision added when it repeats. */
function resultLabels(list: readonly TermResult[]): string[] {
  const base = list.map((r) => {
    const m = /^(\d{4})-(\d{2})/.exec(r.publishedAt ?? "");
    return m ? `${MON[Number(m[2]) - 1]} ${m[1]}` : `Revision ${r.revision}`;
  });
  return base.map((l, i) => (base.filter((x) => x === l).length > 1 && !l.startsWith("Revision") ? `${l} · rev ${list[i]!.revision}` : l));
}

function gradeKind(l: { gradePoint?: string | null; passed: boolean; grade?: string | null }): BadgeKind {
  if (!l.grade) return "neutral";
  if (!l.passed) return "attention";
  const gp = Number(l.gradePoint ?? "0");
  return gp >= 9 ? "done" : gp >= 7 ? "info" : "neutral";
}

/**
 * Student Results (prototype `stu-results.png`): AreaHeader with one tab per published result,
 * CGPA / SGPA / Credits KpiTiles, the grade table, then "Grade cards & reviews" and `aside(span)`
 * (the 6A "My examinations" tile) side by side — no stacked empty tiles.
 */
export function StudentResultsPanel({ aside }: { aside?: (span: 6 | 12) => ReactNode }) {
  const api = useResultsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const results = useQuery({ queryKey: rkeys.scoped("me", "results"), queryFn: () => api.myResults() });
  const cards = useQuery({ queryKey: rkeys.scoped("me", "grade-cards"), queryFn: () => api.myGradeCards() });
  const reviews = useQuery({ queryKey: rkeys.scoped("me", "reviews"), queryFn: () => api.myReviews() });
  const list = [...(results.data ?? [])].sort((a, b) => (a.publishedAt ?? "").localeCompare(b.publishedAt ?? "") || a.revision - b.revision);
  const latest = latestResult(list);
  const labels = resultLabels(list);
  const [picked, setPicked] = useState<string>("");
  const shown = list.find((r) => r.id === picked) ?? latest ?? list[list.length - 1];
  const shownLabel = shown ? labels[list.indexOf(shown)] : undefined;
  const loading = !results.data;
  const cardList = cards.data ?? [];
  const reviewList = reviews.data ?? [];
  const hasSide = cardList.length > 0 || reviewList.length > 0;
  const requestReview = (paperId: string, paperCode: string) =>
    ask({
      title: `Review ${paperCode}`,
      eyebrow: "Results · Request review",
      name: "review-request",
      submitLabel: "Request",
      fields: [
        { name: "kind", label: "Kind", type: "select", required: true, initial: "rechecking", options: [{ value: "rechecking", label: `Rechecking (${formatINR(200)})` }, { value: "revaluation", label: `Revaluation (${formatINR(500)})` }] },
        { name: "reason", label: "Reason", wide: true },
      ],
      onSubmit: (v) => submit(() => api.requestReview({ paperId, kind: v.kind === "revaluation" ? "revaluation" : "rechecking", reason: v.reason }), (x) => `${x.number}: pay ${formatINR(x.feeAmount)} under Fees`),
    });
  return (
    <>
      <AreaHeader
        title="Results"
        summary={loading ? "Loading your results…" : list.length ? `${plural(list.filter((r) => r.status === "published").length, "result")} published by the exam cell${latest?.publishedAt ? ` · latest ${formatDate(latest.publishedAt)}` : ""}` : "Grade cards appear here once the exam cell publishes them"}
        tabs={list.length > 1 ? list.map((r, i) => ({ id: r.id, label: labels[i]! })) : undefined}
        activeTab={shown?.id}
        onTabChange={setPicked}
        tabsLabel="Published results"
        data-page-head="results"
      />
      <KpiTile span={4} icon={Award} tone="violet" label="CGPA" value={dash(latest?.cgpa)} loading={loading} sub={latest ? `${formatNumber(latest.creditsEarned)} credits in the latest result` : "After results are published"} data-kpi="my-cgpa" />
      <KpiTile span={4} icon={TrendingUp} tone="green" label={shownLabel && list.length > 1 ? `${shownLabel} SGPA` : "SGPA"} value={dash(shown?.sgpa)} loading={loading} sub={shown ? statusLabel(shown.outcome) : "Latest published term"} data-kpi="my-sgpa" />
      <KpiTile span={4} icon={Layers} tone="blue" label="Credits earned" value={shown ? formatNumber(shown.creditsEarned) : "—"} loading={loading} sub={shown ? `of ${formatNumber(shown.creditsAttempted)} attempted` : "Per term"} data-kpi="my-credits" />
      <BentoTile
        span={12}
        title={shownLabel && list.length > 1 ? `${shownLabel} grades` : "Grades"}
        data-slice="6C"
        action={shown ? <StatusBadgeInline status={shown.status} /> : undefined}
      >
        {!shown ? (
          <EmptyState icon={GraduationCap} title="No published results" compact>
            Results appear here once they are published.
          </EmptyState>
        ) : (
          <section data-result={shown.id} aria-label={`Result revision ${shown.revision}`}>
            {shown.notice ? <p style={{ margin: "0 0 8px", color: "var(--sb-mute)", fontSize: 14 }}>{shown.notice}</p> : null}
            <DataTable
              caption="Papers"
              rows={shown.lines}
              rowKey={(l) => l.paperId}
              columns={[
                { key: "code", header: "Code", render: (l) => <b>{l.paperCode}</b> },
                { key: "course", header: "Course", render: (l) => (l.remark ? <span>{l.title}<small style={{ display: "block", color: "var(--sb-mute-2)" }}>{l.remark}</small></span> : l.title) },
                { key: "credits", header: "Credits", render: (l) => formatNumber(l.credits), muted: true },
                { key: "marks", header: "Marks", render: (l) => `${dash(l.totalMarks)} / ${formatNumber(l.maxMarks)}`, muted: true },
                { key: "points", header: "Points", render: (l) => (l.gradePoint ? `${formatNumber(l.gradePoint)} pts` : "—"), muted: true },
                { key: "grade", header: "Grade", render: (l) => (l.grade ? <Badge kind={gradeKind(l)}>{l.grade}</Badge> : "—") },
                {
                  key: "review",
                  header: "",
                  render: (l) =>
                    shown.status === "published" ? <RowActions label={`More for ${l.paperCode}`} menu={[{ id: "request-review", label: "Request review", disabled: busy, onSelect: () => requestReview(l.paperId, l.paperCode) }]} /> : null,
                },
              ]}
            />
            <p style={{ margin: "12px 0 0", color: "var(--sb-mute-2)", fontSize: 13 }}>
              SGPA {dash(shown.sgpa)} · CGPA {dash(shown.cgpa)} · {formatNumber(shown.creditsEarned)} / {formatNumber(shown.creditsAttempted)} credits{shown.revision > 1 ? ` · revision ${shown.revision}` : ""}
            </p>
          </section>
        )}
      </BentoTile>
      {hasSide && (
        <BentoTile span={6} title="Grade cards & reviews" data-my-reviews="">
          {cardList.map((c) => (
            <ListRow key={c.id} data-grade-card={c.number} icon={Award} tone="violet" title={`Grade card ${c.number}`} meta={[c.result?.sgpa ? `SGPA ${c.result.sgpa}` : null, `Issued ${formatDate(c.issuedAt)}`].filter(Boolean).join(" · ")} badge={{ label: statusLabel(c.status), kind: statusKind(c.status) }} />
          ))}
          {reviewList.map((x) => {
            const live = ["submitted", "eligibility_verified", "fee_pending", "fee_verified", "approved"].includes(x.status);
            return (
              <ListRow
                key={x.id}
                data-my-review={x.paperCode}
                icon={RotateCcw}
                tone="orange"
                title={`${x.paperCode} · ${statusLabel(x.kind)}`}
                meta={`${x.number} · fee ${formatINR(x.feeAmount)} · ${x.feeSettled ? "paid" : "due"}`}
                badge={{ label: statusLabel(x.status), kind: statusKind(x.status) }}
                menu={live ? [{ id: "withdraw-review", label: "Withdraw", danger: true, disabled: busy, onSelect: () => void run(() => api.withdrawMyReview(x.id), `${x.number} withdrawn`) }] : undefined}
              />
            );
          })}
        </BentoTile>
      )}
      {aside?.(hasSide ? 6 : 12)}
      {prompt}
      {dialog}
    </>
  );
}

function StatusBadgeInline({ status }: { status: string }) {
  return <StatusBadge status={status} />;
}
