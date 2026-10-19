/** ERP_EXTENSIONS 3B: hiring scorecard shapes (mirror the server's people-hr scorecard schemas). */
export type Recommendation = "strong_yes" | "yes" | "no" | "strong_no";
export type Leaning = "hire" | "no_hire" | "split";
export type InterviewType = "phone" | "video" | "onsite";

export interface RatingField { key: string; label: string }
export interface GuideQuestion { id: string; text: string; field?: string }
export interface Guide { id: string; name: string; questions: GuideQuestion[]; types: InterviewType[]; stages: string[] }
export interface GuideAnswer { question: string; answer: string; field?: string }
export interface ScreeningQuestion { id: string; label: string; kind: "short" | "long" | "yesno" | "choice"; required: boolean; mustHave?: boolean; options?: string[] }
export interface ScreeningAnswer { label: string; answer: string }

export interface HiringSettings { ratingFields: RatingField[] | null; guides: Guide[] }
export interface HiringForm { requisitionId: string; ratingFields: RatingField[] | null; rubric: RatingField[]; screeningQuestions: ScreeningQuestion[] }

export interface ScorecardSummary {
  count: number;
  average: number | null;
  byField: Record<string, number | null>;
  recommendations: Record<Recommendation, number>;
  leaning: Leaning | null;
}

export interface CandidateInterview {
  id: string; candidateId: string; type: InterviewType; date: string; start: string; end: string;
  interviewers: string[]; location: string | null; guide: Guide | null; status: "scheduled" | "cancelled"; hasScorecard: boolean;
}
export interface CandidateInterviewInput { type: InterviewType; date: string; start: string; end: string; interviewers?: string[]; location?: string | null; guideId?: string | null }

export interface Scorecard {
  id: string; candidateId: string; interviewId: string | null; authorName: string; recommendation: Recommendation;
  ratings: Record<string, number>; fields: RatingField[]; guideAnswers: GuideAnswer[]; notes: string | null; average: number | null; createdOn: string;
}
export interface ScorecardInput { interviewId?: string | null; authorName: string; recommendation: Recommendation; ratings: Record<string, number | null>; guideAnswers?: GuideAnswer[]; notes?: string | null }

export interface CandidateFeedback {
  candidateId: string; name: string; status: string; requisitionTitle: string; rubric: RatingField[];
  interviews: CandidateInterview[]; scorecards: Scorecard[]; summary: ScorecardSummary;
  screening: { questions: ScreeningQuestion[]; answers: ScreeningAnswer[]; mismatches: string[] };
}
export interface CompareRow { candidateId: string; name: string; status: string; summary: ScorecardSummary; screeningFlags: number }
export interface ScorecardCompare { requisitionId: string; rubric: RatingField[]; items: CompareRow[] }
export interface FeedbackDueRow { interviewId: string; candidateId: string; candidateName: string; requisitionTitle: string; date: string; type: InterviewType; interviewers: string[] }
