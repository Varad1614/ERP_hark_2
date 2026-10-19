/** ERP_EXTENSIONS 3B: display helpers for hiring scorecards (the server computes the numbers). */
import type { InterviewType, Leaning, RatingField, Recommendation, ScorecardSummary } from "./types";

export const DEFAULT_RUBRIC: RatingField[] = [
  { key: "skills", label: "Role skills" },
  { key: "experience", label: "Experience" },
  { key: "communication", label: "Communication" },
  { key: "collaboration", label: "Collaboration" },
];

/** A starting rubric for teaching posts; the institution can edit it. */
export const FACULTY_RUBRIC: RatingField[] = [
  { key: "subject_knowledge", label: "Subject knowledge" },
  { key: "teaching_demo", label: "Teaching demonstration" },
  { key: "research", label: "Research and publications" },
  { key: "communication", label: "Communication" },
  { key: "institution_fit", label: "Fit with the department" },
];

export const RECOMMENDATIONS: { value: Recommendation; label: string; score: number }[] = [
  { value: "strong_yes", label: "Strong yes", score: 2 },
  { value: "yes", label: "Yes", score: 1 },
  { value: "no", label: "No", score: -1 },
  { value: "strong_no", label: "Strong no", score: -2 },
];

export const INTERVIEW_TYPES: { value: InterviewType; label: string }[] = [
  { value: "phone", label: "Phone screen" },
  { value: "video", label: "Video call" },
  { value: "onsite", label: "Onsite" },
];

export const recommendationLabel = (r: Recommendation) => RECOMMENDATIONS.find((x) => x.value === r)?.label ?? r;
export const interviewTypeLabel = (t: InterviewType) => INTERVIEW_TYPES.find((x) => x.value === t)?.label ?? t;

export function leaningLabel(l: Leaning | null): string {
  if (l === "hire") return "Leaning hire";
  if (l === "no_hire") return "Leaning no hire";
  if (l === "split") return "Split panel";
  return "No feedback yet";
}

/** "4.3 / 5", or a dash when nobody rated. */
export const ratingText = (v: number | null | undefined) => (v == null ? "—" : `${(Math.round(v * 10) / 10).toString()} / 5`);

/** "2 strong yes · 1 no". */
export function tallyText(s: ScorecardSummary): string {
  const parts = RECOMMENDATIONS.filter((r) => (s.recommendations[r.value] ?? 0) > 0).map((r) => `${s.recommendations[r.value]} ${r.label.toLowerCase()}`);
  return parts.join(" · ") || "—";
}

/** Turns a key from a label: "Teaching demo" → "teaching_demo". */
export function keyFor(label: string): string {
  return label.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "").slice(0, 64) || "field";
}

/** Keeps a 1–5 integer, or null. */
export function toRating(s: string): number | null {
  const n = Number(s);
  return Number.isInteger(n) && n >= 1 && n <= 5 ? n : null;
}
