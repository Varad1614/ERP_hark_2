/** In-memory `HiringApi` for mock mode and screen tests (the server's rules are authoritative). */
import type { HiringApi } from "./api";
import { DEFAULT_RUBRIC, RECOMMENDATIONS } from "./scorecards";
import type { CandidateInterview, HiringSettings, RatingField, Scorecard, ScorecardSummary, ScreeningAnswer, ScreeningQuestion } from "./types";

const valid = (v: unknown): v is number => Number.isInteger(v) && (v as number) >= 1 && (v as number) <= 5;
const mean = (v: number[]) => (v.length ? v.reduce((a, b) => a + b, 0) / v.length : null);
const avg = (s: { ratings: Record<string, number> }) => mean(Object.values(s.ratings).filter(valid));

export function summarize(cards: Scorecard[], fields: RatingField[]): ScorecardSummary {
  const s: ScorecardSummary = { count: cards.length, average: null, byField: {}, recommendations: { strong_yes: 0, yes: 0, no: 0, strong_no: 0 }, leaning: null };
  if (!cards.length) return s;
  for (const f of fields) s.byField[f.key] = mean(cards.map((c) => c.ratings[f.key]).filter(valid));
  s.average = mean(cards.map(avg).filter((v): v is number => v !== null));
  let total = 0;
  for (const c of cards) {
    const r = RECOMMENDATIONS.find((x) => x.value === c.recommendation);
    if (!r) continue;
    s.recommendations[r.value] += 1;
    total += r.score;
  }
  s.leaning = total > 0 ? "hire" : total < 0 ? "no_hire" : "split";
  return s;
}

export function createMockHiringApi(): HiringApi {
  const candidates = [
    { id: "cand-1", name: "Priya Kulkarni", status: "shortlisted", requisitionId: "req-1" },
    { id: "cand-2", name: "Rohan Joshi", status: "applied", requisitionId: "req-1" },
  ];
  let settings: HiringSettings = { ratingFields: null, guides: [] };
  let form: { ratingFields: RatingField[] | null; screeningQuestions: ScreeningQuestion[] } = { ratingFields: null, screeningQuestions: [] };
  const answers = new Map<string, ScreeningAnswer[]>();
  const interviews: CandidateInterview[] = [];
  const cards: Scorecard[] = [];
  let seq = 0;
  const rubric = () => form.ratingFields ?? settings.ratingFields ?? DEFAULT_RUBRIC;
  const today = () => new Date().toISOString().slice(0, 10);
  const mismatches = (cid: string) =>
    form.screeningQuestions.filter((q) => q.mustHave).filter((q) => {
      const a = (answers.get(cid) ?? []).find((x) => x.label.trim().toLowerCase() === q.label.trim().toLowerCase());
      return a && !/^(yes|y|true)$/i.test(a.answer.trim());
    }).map((q) => q.id);
  return {
    settings: async () => settings,
    saveSettings: async (_id, s) => (settings = s),
    saveForm: async (id, f) => {
      form = {
        ratingFields: f.ratingFields === undefined ? form.ratingFields : f.ratingFields?.length ? f.ratingFields : null,
        screeningQuestions: f.screeningQuestions === undefined ? form.screeningQuestions : (f.screeningQuestions ?? []),
      };
      return { requisitionId: id, ratingFields: form.ratingFields, rubric: rubric(), screeningQuestions: form.screeningQuestions };
    },
    saveAnswers: async (id, a) => {
      answers.set(id, a.filter((x) => x.label.trim() && x.answer.trim()));
    },
    scheduleInterview: async (cid, i) => {
      if (i.end <= i.start) throw new Error("End after start.");
      const guide = settings.guides.find((g) => g.id === i.guideId) ?? settings.guides.find((g) => g.types.includes(i.type)) ?? null;
      const v: CandidateInterview = { id: `itv-${++seq}`, candidateId: cid, type: i.type, date: i.date, start: i.start, end: i.end, interviewers: i.interviewers ?? [], location: i.location ?? null, guide, status: "scheduled", hasScorecard: false };
      interviews.push(v);
      return v;
    },
    cancelInterview: async (id) => {
      const i = interviews.find((x) => x.id === id);
      if (i) i.status = "cancelled";
    },
    createScorecard: async (cid, input) => {
      if (!input.authorName.trim()) throw new Error("Add the reviewer’s name.");
      const ratings = Object.fromEntries(Object.entries(input.ratings).filter((e): e is [string, number] => valid(e[1])));
      const s: Scorecard = { id: `sc-${++seq}`, candidateId: cid, interviewId: input.interviewId ?? null, authorName: input.authorName.trim(), recommendation: input.recommendation, ratings, fields: rubric(), guideAnswers: input.guideAnswers ?? [], notes: input.notes ?? null, average: avg({ ratings }), createdOn: today() };
      cards.push(s);
      return s;
    },
    feedback: async (cid) => {
      const c = candidates.find((x) => x.id === cid) ?? candidates[0]!;
      const mine = cards.filter((s) => s.candidateId === cid);
      return {
        candidateId: cid, name: c.name, status: c.status, requisitionTitle: "Assistant Professor, Commerce", rubric: rubric(),
        interviews: interviews.filter((i) => i.candidateId === cid).map((i) => ({ ...i, hasScorecard: mine.some((s) => s.interviewId === i.id) })),
        scorecards: mine, summary: summarize(mine, rubric()),
        screening: { questions: form.screeningQuestions, answers: answers.get(cid) ?? [], mismatches: mismatches(cid) },
      };
    },
    compare: async (rid) => ({
      requisitionId: rid, rubric: rubric(),
      items: candidates.map((c) => ({ candidateId: c.id, name: c.name, status: c.status, summary: summarize(cards.filter((s) => s.candidateId === c.id), rubric()), screeningFlags: mismatches(c.id).length })),
    }),
    feedbackDue: async () =>
      interviews
        .filter((i) => i.status === "scheduled" && i.date < today())
        .filter((i) => !cards.some((s) => s.interviewId === i.id || (s.candidateId === i.candidateId && !s.interviewId && s.createdOn >= i.date)))
        .map((i) => ({ interviewId: i.id, candidateId: i.candidateId, candidateName: candidates.find((c) => c.id === i.candidateId)?.name ?? "", requisitionTitle: "Assistant Professor, Commerce", date: i.date, type: i.type, interviewers: i.interviewers })),
  };
}
