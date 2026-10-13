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
import { Award, FileCheck2, GraduationCap, ListChecks, RotateCcw } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Select, StatTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, NoInstitution, REASON, useAsk, type RowAction } from "../../institution/forms";
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
  const picker = (
    <Select
      label="Examination session"
      value={sessionId}
      onChange={(e) => setPicked(e.target.value)}
      options={list.map((s) => ({ value: s.id, label: `${s.code} · ${s.name} (${statusLabel(s.status)})` }))}
      data-field="session"
    />
  );
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

  return (
    <>
      <BentoTile span={12} title="Result computation" data-slice="6C" data-result-batch action={
        enabled && !batch ? (
          <Button size="sm" variant="ghost" data-action="new-batch" disabled={busy} onClick={() => run(() => api.createBatch(sessionId), "Result batch opened")}>
            Open batch
          </Button>
        ) : undefined
      }>
        {picker}
        {empty ? (
          <EmptyState icon={ListChecks} title="No examination sessions">Sessions are set up under Sessions & policy.</EmptyState>
        ) : batch ? (
          <DataTable caption="Result batch" rows={[batch]} rowKey={(b) => b.id}
            columns={[
              { key: "status", header: "Status", render: (b) => <StatusBadge status={b.status} /> },
              { key: "runs", header: "Runs", render: (b) => String(b.runCount) },
              { key: "computed", header: "Computed", render: (b) => String(b.computedCount) },
              { key: "withheld", header: "Withheld", render: (b) => String(b.withheldCount) },
              { key: "reason", header: "Last run", render: (b) => dash(b.statusReason), muted: true },
              {
                key: "actions",
                header: "",
                render: (b) => {
                  const acts: RowAction[] = batchActions(b).map((c) => ({ id: `batch-${c}`, label: statusLabel(c), variant: c === "validate" ? "primary" : "ghost", onClick: () => run(() => api.batchCommand(b.id, b.version, c), `Batch: ${statusLabel(c)}`) }));
                  if (b.status === "validated" && b.processInstanceId) {
                    acts.push({ id: "batch-approve", label: "Approve", variant: "primary", onClick: () => run(() => api.decideBatch(b.id, b.version, "approve"), "Results approved") });
                    acts.push({ id: "batch-reject", label: "Reject", onClick: () => ask({ title: "Reject results", name: "batch-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideBatch(b.id, b.version, "reject", v.reason), "Results sent back") }) });
                  }
                  return <Actions actions={acts} disabled={busy} />;
                },
              },
            ]}
          />
        ) : (
          <EmptyState icon={ListChecks} title="No result batch yet">Open a batch, then compute results from final marks.</EmptyState>
        )}
      </BentoTile>
      <BentoTile span={12} title="Results" data-results>
        <DataTable caption="Term results" rows={results.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={GraduationCap} title="No results">Compute the batch to produce results.</EmptyState>}
          columns={[
            { key: "student", header: "Candidate", render: (r) => `${r.studentNumber} · ${r.displayName}` },
            { key: "rev", header: "Rev.", render: (r) => String(r.revision), muted: true },
            { key: "credits", header: "Credits", render: (r) => `${r.creditsEarned} / ${r.creditsAttempted}` },
            { key: "sgpa", header: "SGPA", render: (r) => dash(r.sgpa) },
            { key: "cgpa", header: "CGPA", render: (r) => dash(r.cgpa) },
            { key: "outcome", header: "Outcome", render: (r) => <StatusBadge status={r.outcome} /> },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "reason", header: "Note", render: (r) => dash(r.statusReason), muted: true },
            { key: "actions", header: "", render: (r) => <Actions disabled={busy} actions={resultActions(r).map((a) => ({ id: `result-${a}`, label: a === "card" ? "Grade card" : statusLabel(a), onClick: () => void onResult(r, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Publication" data-publications action={
        enabled ? (
          <Button size="sm" variant="ghost" data-action="new-publication" disabled={busy} onClick={() => run(() => api.createPublication(sessionId), "Publication planned")}>
            Plan publication
          </Button>
        ) : undefined
      }>
        <DataTable caption="Publications" rows={pubs.data ?? []} rowKey={(p) => p.id}
          empty={<EmptyState icon={FileCheck2} title="No publications">Plan one once the results are approved.</EmptyState>}
          columns={[
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "published", header: "Published", render: (p) => String(p.publishedCount) },
            { key: "skipped", header: "Withheld", render: (p) => String(p.skippedCount) },
            { key: "note", header: "Note", render: (p) => dash(p.lastError ?? p.statusReason), muted: true },
            { key: "actions", header: "", render: (p) => <Actions disabled={busy} actions={publicationActions(p).map((a) => ({ id: `publication-${a}`, label: statusLabel(a), variant: a === "publish" ? "primary" : "ghost", onClick: () => void onPublication(p, a) }))} /> },
          ]}
        />
      </BentoTile>
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

  return (
    <>
      <BentoTile span={12} title="Revaluation & rechecking" data-slice="6C" data-reviews>
        {picker}
        <DataTable caption="Review requests" rows={reviews.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={RotateCcw} title="No review requests">Students request reviews of published results.</EmptyState>}
          columns={[
            { key: "number", header: "Request", render: (r) => `${r.number} · ${statusLabel(r.kind)}` },
            { key: "student", header: "Candidate", render: (r) => `${r.studentNumber} · ${r.displayName}` },
            { key: "paper", header: "Paper", render: (r) => r.paperCode },
            { key: "fee", header: "Fee", render: (r) => `₹${r.feeAmount} · ${r.feeSettled ? "paid" : dash(r.feeChargeNumber)}` },
            { key: "outcome", header: "Marks", render: (r) => (r.outcome ? `${dash(r.outcome.fromMarks)} → ${dash(r.outcome.toMarks)}` : "—") },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions disabled={busy} actions={reviewActions(r).map((a) => ({ id: `review-${a}`, label: statusLabel(a), onClick: () => void onReview(r, a) }))} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={7} title="Supplementary eligibility" data-eligibility action={
        enabled ? (
          <Button size="sm" variant="ghost" data-action="determine-eligibility" disabled={busy} onClick={() => run(() => api.determineEligibility(sessionId), (s) => `${s.eligible} eligible, ${s.exhausted} exhausted`)}>
            Determine
          </Button>
        ) : undefined
      }>
        <DataTable caption="Subsequent attempts" rows={elig.data ?? []} rowKey={(e) => e.id}
          empty={<EmptyState icon={ListChecks} title="No eligibility yet">Determine it from published failed papers.</EmptyState>}
          columns={[
            { key: "student", header: "Candidate", render: (e) => `${e.studentNumber} · ${e.displayName}` },
            { key: "paper", header: "Paper", render: (e) => e.paperCode },
            { key: "attempts", header: "Attempts", render: (e) => `${e.attemptsUsed} / ${e.maxAttempts}` },
            { key: "best", header: "Best", render: (e) => `${dash(e.bestPercent)} ${e.bestGrade ?? ""}` },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={5} title="Session closure" data-closure action={<Actions actions={enabled ? closureActs : []} disabled={busy} />}>
        {c ? (
          <DataTable caption="Closure" rows={[c]} rowKey={(x) => x.id}
            columns={[
              { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
              { key: "blockers", header: "Blockers", render: (x) => (x.checks.blockers && x.checks.blockers.length > 0 ? x.checks.blockers.join(", ") : "none") },
              { key: "completed", header: "Completed", render: (x) => String(x.completedRegistrations) },
            ]}
          />
        ) : (
          <EmptyState icon={Award} title="Not closing">Close the session once results are published and reviews are done.</EmptyState>
        )}
      </BentoTile>
      <BentoTile span={12} title="Completion evaluations" data-completions action={
        <Button size="sm" variant="ghost" data-action="evaluate-completion" disabled={busy} onClick={() => ask({ title: "Evaluate completion", name: "completion", submitLabel: "Evaluate", fields: [{ name: "studentId", label: "Student id", required: true, wide: true }], onSubmit: (v) => submit(() => api.evaluateCompletion(institution.id, v.studentId!.trim()), (r) => `${r.studentNumber}: ${statusLabel(r.status)}`) })}>
          Evaluate student
        </Button>
      }>
        <DataTable caption="Completion" rows={completions.data ?? []} rowKey={(x) => x.id}
          empty={<EmptyState icon={GraduationCap} title="No evaluations">Evaluate a student's credits and CGPA against the curriculum.</EmptyState>}
          columns={[
            { key: "student", header: "Student", render: (x) => `${x.studentNumber} · ${x.displayName}` },
            { key: "credits", header: "Credits", render: (x) => `${x.creditsEarned} / ${x.creditsRequired}` },
            { key: "components", header: "Components", render: (x) => `${x.componentsPassed} / ${x.componentsRequired}` },
            { key: "cgpa", header: "CGPA", render: (x) => dash(x.cgpa) },
            { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
          ]}
        />
      </BentoTile>
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

export function StudentResultsPanel() {
  const api = useResultsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const results = useQuery({ queryKey: rkeys.scoped("me", "results"), queryFn: () => api.myResults() });
  const cards = useQuery({ queryKey: rkeys.scoped("me", "grade-cards"), queryFn: () => api.myGradeCards() });
  const reviews = useQuery({ queryKey: rkeys.scoped("me", "reviews"), queryFn: () => api.myReviews() });
  const list = results.data ?? [];
  const latest = latestResult(list);
  const loading = !results.data;
  return (
    <>
    <StatTile span={4} icon={Award} tone="green" label="CGPA" value={loading ? "…" : dash(latest?.cgpa)} hint={latest ? `Revision ${latest.revision}` : "After results are published"} data-kpi="my-cgpa" />
    <StatTile span={4} icon={GraduationCap} tone="violet" label="SGPA" value={loading ? "…" : dash(latest?.sgpa)} hint={latest ? statusLabel(latest.outcome) : "Latest published term"} data-kpi="my-sgpa" />
    <StatTile span={4} icon={RotateCcw} tone="orange" label="Credits earned" value={loading ? "…" : latest ? latest.creditsEarned : "—"} hint={latest ? `of ${latest.creditsAttempted} attempted` : "Per term"} data-kpi="my-credits" />
    <BentoTile span={8} title="Results" data-slice="6C">
      {list.length === 0 ? (
        <EmptyState icon={GraduationCap} title="No published results">Results appear here once they are published.</EmptyState>
      ) : (
        list.map((r) => (
          <section key={r.id} data-result={r.id} aria-label={`Result revision ${r.revision}`}>
            <h3>
              SGPA {dash(r.sgpa)} · CGPA {dash(r.cgpa)} · {r.creditsEarned} / {r.creditsAttempted} credits <StatusBadge status={r.status} />
            </h3>
            {r.notice ? <p>{r.notice}</p> : null}
            <DataTable caption="Papers" rows={r.lines} rowKey={(l) => l.paperId}
              columns={[
                { key: "paper", header: "Paper", render: (l) => `${l.paperCode} — ${l.title}` },
                { key: "credits", header: "Credits", render: (l) => l.credits },
                { key: "marks", header: "Marks", render: (l) => `${dash(l.totalMarks)} / ${l.maxMarks}` },
                { key: "percent", header: "%", render: (l) => dash(l.percent) },
                { key: "grade", header: "Grade", render: (l) => dash(l.grade) },
                { key: "remark", header: "Remark", render: (l) => dash(l.remark), muted: true },
                {
                  key: "review",
                  header: "",
                  render: (l) =>
                    r.status === "published" ? (
                      <Actions disabled={busy} actions={[{ id: "request-review", label: "Request review", onClick: () => ask({
                        title: `Review ${l.paperCode}`,
                        name: "review-request",
                        submitLabel: "Request",
                        fields: [
                          { name: "kind", label: "Kind", type: "select", required: true, initial: "rechecking", options: [{ value: "rechecking", label: "Rechecking (₹200.00)" }, { value: "revaluation", label: "Revaluation (₹500.00)" }] },
                          { name: "reason", label: "Reason", wide: true },
                        ],
                        onSubmit: (v) => submit(() => api.requestReview({ paperId: l.paperId, kind: v.kind === "revaluation" ? "revaluation" : "rechecking", reason: v.reason }), (x) => `${x.number}: pay ₹${x.feeAmount} under Fees`),
                      }) }]} />
                    ) : null,
                },
              ]}
            />
          </section>
        ))
      )}
      <DataTable caption="Grade cards" rows={cards.data ?? []} rowKey={(c) => c.id}
        empty={<EmptyState icon={Award} title="No grade cards">Grade cards are issued after publication.</EmptyState>}
        columns={[
          { key: "number", header: "Grade card", render: (c) => c.number },
          { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
          { key: "sgpa", header: "SGPA", render: (c) => dash(c.result?.sgpa) },
        ]}
      />
      <DataTable caption="Review requests" rows={reviews.data ?? []} rowKey={(x) => x.id}
        empty={<EmptyState icon={RotateCcw} title="No review requests">Request a rechecking or revaluation of a published paper.</EmptyState>}
        columns={[
          { key: "number", header: "Request", render: (x) => `${x.number} · ${statusLabel(x.kind)}` },
          { key: "paper", header: "Paper", render: (x) => x.paperCode },
          { key: "fee", header: "Fee", render: (x) => `₹${x.feeAmount} · ${x.feeSettled ? "paid" : "due"}` },
          { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
          {
            key: "actions",
            header: "",
            render: (x) =>
              ["submitted", "eligibility_verified", "fee_pending", "fee_verified", "approved"].includes(x.status) ? (
                <Actions disabled={busy} actions={[{ id: "withdraw-review", label: "Withdraw", onClick: () => void run(() => api.withdrawMyReview(x.id), `${x.number} withdrawn`) }]} />
              ) : null,
          },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
    </>
  );
}
