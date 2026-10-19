/**
 * ERP_EXTENSIONS 3B: hiring scorecards (ported from Everything's hiring app, MIT). Compare a
 * requisition's candidates on panel feedback, open one candidate's interviews and scorecards,
 * write a scorecard against the rubric, see feedback still owed, and set the rubric and
 * screening questions.
 */
import { ClipboardCheck } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import { createHttpHiringApi, type HiringApi } from "./api";
import { createMockHiringApi } from "./mock";
import { DEFAULT_RUBRIC, FACULTY_RUBRIC, INTERVIEW_TYPES, interviewTypeLabel, keyFor, leaningLabel, ratingText, RECOMMENDATIONS, recommendationLabel, tallyText, toRating } from "./scorecards";
import type { InterviewType, RatingField, Recommendation } from "./types";

let fallback: HiringApi | null = null;
export function defaultHiringApi(): HiringApi {
  fallback ??= USES_API ? createHttpHiringApi() : createMockHiringApi();
  return fallback;
}

type Tab = "compare" | "candidate" | "due" | "rubric";
const TABS: { value: Tab; label: string }[] = [
  { value: "compare", label: "Compare" },
  { value: "candidate", label: "Feedback" },
  { value: "due", label: "Feedback due" },
  { value: "rubric", label: "Rubric" },
];

const leaningKind = (l: string | null) => (l === "hire" ? "done" : l === "no_hire" ? "attention" : "info");
const today = () => new Date().toISOString().slice(0, 10);

export function HiringScorecardsTile({ institutionId, requisitionId, api = defaultHiringApi() }: { institutionId: string; requisitionId: string; api?: HiringApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("compare");
  const [candidateId, setCandidateId] = useState("");
  const [card, setCard] = useState<{ author: string; recommendation: Recommendation | ""; interviewId: string; notes: string; ratings: Record<string, string> }>({ author: "", recommendation: "", interviewId: "", notes: "", ratings: {} });
  const [itv, setItv] = useState<{ type: InterviewType; date: string; start: string; end: string; interviewers: string }>({ type: "onsite", date: today(), start: "11:00", end: "11:45", interviewers: "" });
  const [rubricDraft, setRubricDraft] = useState<RatingField[] | null>(null);
  const [newField, setNewField] = useState("");
  // Prefix `institution`: `useCommand` refreshes every `institution` query after a command.
  const key = (...p: string[]) => ["institution", "hiring", ...p];
  const compare = useQuery({ queryKey: key("compare", requisitionId), queryFn: () => api.compare(requisitionId), enabled: requisitionId !== "" && (tab === "compare" || tab === "rubric" || tab === "candidate") });
  const feedback = useQuery({ queryKey: key("feedback", candidateId), queryFn: () => api.feedback(candidateId), enabled: tab === "candidate" && candidateId !== "" });
  const due = useQuery({ queryKey: key("due", institutionId), queryFn: () => api.feedbackDue(institutionId), enabled: tab === "due" });
  const rubric = rubricDraft ?? compare.data?.rubric ?? DEFAULT_RUBRIC;
  const f = feedback.data;

  const open = (id: string) => {
    setCandidateId(id);
    setTab("candidate");
  };
  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-hiring-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );

  return (
    <BentoTile span={12} title="Interview scorecards" data-slice="3B" data-hiring-scorecards action={tabs}>
      {tab === "compare" && (requisitionId === "" ? (
        <EmptyState icon={ClipboardCheck} title="Pick a requisition" compact>Choose one in Candidates to compare its applicants.</EmptyState>
      ) : (
        <DataTable caption="Candidates by panel feedback" rows={compare.data?.items ?? []} rowKey={(r) => r.candidateId}
          empty={<EmptyState icon={ClipboardCheck} title="No candidates" compact>Add applicants to this requisition.</EmptyState>}
          columns={[
            { key: "n", header: "Candidate", render: (r) => r.name },
            { key: "l", header: "Panel", render: (r) => <Badge kind={leaningKind(r.summary.leaning)}>{leaningLabel(r.summary.leaning)}</Badge> },
            { key: "a", header: "Average", render: (r) => ratingText(r.summary.average) },
            { key: "t", header: "Recommendations", render: (r) => tallyText(r.summary) },
            { key: "s", header: "Screening", render: (r) => (r.screeningFlags ? <Badge kind="attention">{`${r.screeningFlags} must-have not met`}</Badge> : "—") },
            { key: "o", header: "", render: (r) => <Button size="sm" variant="ghost" data-action="open-feedback" onClick={() => open(r.candidateId)}>Open</Button> },
          ]} />
      ))}

      {tab === "candidate" && (
        <>
          <FormSelect label="Candidate" value={candidateId} onChange={(e) => setCandidateId(e.target.value)} data-field="scorecard-candidate"
            options={[{ value: "", label: "Choose a candidate" }, ...(compare.data?.items ?? []).map((r) => ({ value: r.candidateId, label: r.name }))]} />
          {f && (
            <>
              <KeyValue label={f.name} value={`${leaningLabel(f.summary.leaning)} · average ${ratingText(f.summary.average)} · ${f.summary.count} scorecard${f.summary.count === 1 ? "" : "s"}`} />
              <DataTable caption="By dimension" rows={f.rubric} rowKey={(r) => r.key}
                columns={[
                  { key: "l", header: "Dimension", render: (r) => r.label },
                  { key: "v", header: "Panel average", render: (r) => ratingText(f.summary.byField[r.key]) },
                ]} />
              {f.screening.mismatches.length > 0 && (
                <p role="alert">{`Must-have not met: ${f.screening.questions.filter((q) => f.screening.mismatches.includes(q.id)).map((q) => q.label).join(", ")}`}</p>
              )}
              <DataTable caption="Interviews" rows={f.interviews} rowKey={(i) => i.id}
                empty={<EmptyState icon={ClipboardCheck} title="No interviews" compact>Schedule one below.</EmptyState>}
                columns={[
                  { key: "d", header: "When", render: (i) => `${i.date} ${i.start}–${i.end}` },
                  { key: "t", header: "Format", render: (i) => interviewTypeLabel(i.type) },
                  { key: "p", header: "Panel", render: (i) => i.interviewers.join(", ") || "—" },
                  { key: "g", header: "Guide", render: (i) => i.guide?.name ?? "—" },
                  { key: "s", header: "", render: (i) => (i.status === "cancelled" ? <Badge kind="info">Cancelled</Badge> : i.hasScorecard ? <Badge kind="done">Feedback in</Badge> : (
                    <Button size="sm" variant="ghost" data-action="cancel-interview" disabled={busy} onClick={() => void run(() => api.cancelInterview(i.id), "Interview cancelled")}>Cancel</Button>
                  )) },
                ]} />
              <FormGrid columns={3}>
                <FormSelect label="Format" value={itv.type} onChange={(e) => setItv({ ...itv, type: e.target.value as InterviewType })} options={INTERVIEW_TYPES} />
                <TextField label="Date" type="date" value={itv.date} onChange={(e) => setItv({ ...itv, date: e.target.value })} />
                <TextField label="Panel (comma separated)" value={itv.interviewers} placeholder="Dr. Rao, Prof. Iyer" onChange={(e) => setItv({ ...itv, interviewers: e.target.value })} />
                <TextField label="Starts" type="time" value={itv.start} onChange={(e) => setItv({ ...itv, start: e.target.value })} />
                <TextField label="Ends" type="time" value={itv.end} onChange={(e) => setItv({ ...itv, end: e.target.value })} />
              </FormGrid>
              <Button size="sm" variant="ghost" data-action="schedule-interview" disabled={busy || itv.end <= itv.start} onClick={() => void run(() => api.scheduleInterview(candidateId, { type: itv.type, date: itv.date, start: itv.start, end: itv.end, interviewers: itv.interviewers.split(",").map((s) => s.trim()).filter(Boolean) }), "Interview scheduled")}>Schedule interview</Button>

              <DataTable caption="Scorecards" rows={f.scorecards} rowKey={(s) => s.id}
                empty={<EmptyState icon={ClipboardCheck} title="No feedback yet" compact>Write the first scorecard below.</EmptyState>}
                columns={[
                  { key: "a", header: "Reviewer", render: (s) => s.authorName },
                  { key: "r", header: "Recommendation", render: (s) => recommendationLabel(s.recommendation) },
                  { key: "v", header: "Average", render: (s) => ratingText(s.average) },
                  { key: "d", header: "Ratings", render: (s) => s.fields.filter((x) => s.ratings[x.key] != null).map((x) => `${x.label} ${s.ratings[x.key]}`).join(" · ") || "—" },
                  { key: "n", header: "Notes", render: (s) => s.notes ?? "—" },
                ]} />
              <FormGrid columns={3}>
                <TextField label="Reviewer" value={card.author} onChange={(e) => setCard({ ...card, author: e.target.value })} />
                <FormSelect label="Recommendation" value={card.recommendation} onChange={(e) => setCard({ ...card, recommendation: e.target.value as Recommendation })}
                  options={[{ value: "", label: "Choose" }, ...RECOMMENDATIONS.map((r) => ({ value: r.value, label: r.label }))]} />
                <FormSelect label="Interview" value={card.interviewId} onChange={(e) => setCard({ ...card, interviewId: e.target.value })}
                  options={[{ value: "", label: "Not tied to one" }, ...f.interviews.filter((i) => i.status === "scheduled").map((i) => ({ value: i.id, label: `${i.date} · ${interviewTypeLabel(i.type)}` }))]} />
                {f.rubric.map((r) => (
                  <TextField key={r.key} label={`${r.label} (1–5)`} inputMode="numeric" value={card.ratings[r.key] ?? ""} onChange={(e) => setCard({ ...card, ratings: { ...card.ratings, [r.key]: e.target.value } })} />
                ))}
                <TextField label="Notes" value={card.notes} onChange={(e) => setCard({ ...card, notes: e.target.value })} />
              </FormGrid>
              <Button size="sm" data-action="save-scorecard" disabled={busy || !card.author.trim() || !card.recommendation} onClick={() => void run(async () => {
                await api.createScorecard(candidateId, {
                  authorName: card.author.trim(), recommendation: card.recommendation as Recommendation, interviewId: card.interviewId || null, notes: card.notes.trim() || null,
                  ratings: Object.fromEntries(f.rubric.map((r) => [r.key, toRating(card.ratings[r.key] ?? "")])),
                });
                setCard({ author: "", recommendation: "", interviewId: "", notes: "", ratings: {} });
              }, "Scorecard saved")}>Save scorecard</Button>
            </>
          )}
        </>
      )}

      {tab === "due" && (
        <DataTable caption="Interviews without feedback" rows={due.data ?? []} rowKey={(r) => r.interviewId}
          empty={<EmptyState icon={ClipboardCheck} title="All feedback is in" compact>Interviews older than a day show here until someone writes a scorecard.</EmptyState>}
          columns={[
            { key: "c", header: "Candidate", render: (r) => r.candidateName },
            { key: "q", header: "Requisition", render: (r) => r.requisitionTitle },
            { key: "d", header: "Held on", render: (r) => r.date },
            { key: "p", header: "Panel", render: (r) => r.interviewers.join(", ") || "—" },
            { key: "o", header: "", render: (r) => <Button size="sm" variant="ghost" data-action="open-due" onClick={() => open(r.candidateId)}>Write feedback</Button> },
          ]} />
      )}

      {tab === "rubric" && (requisitionId === "" ? (
        <EmptyState icon={ClipboardCheck} title="Pick a requisition" compact>Each requisition can have its own rubric.</EmptyState>
      ) : (
        <>
          <DataTable caption="Rating dimensions" rows={rubric} rowKey={(r) => r.key}
            columns={[
              { key: "l", header: "Dimension", render: (r) => r.label },
              { key: "x", header: "", render: (r) => <Button size="sm" variant="ghost" data-action="remove-dimension" onClick={() => setRubricDraft(rubric.filter((x) => x.key !== r.key))}>Remove</Button> },
            ]} />
          <div className="flex gap-2 items-end">
            <TextField label="Add a dimension" value={newField} placeholder="Teaching demonstration" onChange={(e) => setNewField(e.target.value)} />
            <Button size="sm" variant="ghost" data-action="add-dimension" disabled={!newField.trim() || rubric.some((r) => r.key === keyFor(newField))} onClick={() => {
              setRubricDraft([...rubric, { key: keyFor(newField), label: newField.trim() }]);
              setNewField("");
            }}>Add</Button>
            <Button size="sm" variant="ghost" data-action="faculty-rubric" onClick={() => setRubricDraft(FACULTY_RUBRIC)}>Use the faculty rubric</Button>
          </div>
          <Button size="sm" data-action="save-rubric" disabled={busy || !rubricDraft} onClick={() => void run(async () => {
            await api.saveForm(requisitionId, { ratingFields: rubricDraft!.length ? rubricDraft : null });
            setRubricDraft(null);
          }, "Rubric saved")}>Save rubric</Button>
        </>
      ))}
      {dialog}
    </BentoTile>
  );
}
