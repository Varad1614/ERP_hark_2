/** Slice 6C wire types (decimals are strings, exactly as the API sends them). */

export interface Items<T> {
  items: T[];
}

export interface ExamSessionRef {
  id: string;
  code: string;
  name: string;
  status: string;
  kind?: string;
}

export interface ResultLine {
  lineNo: number;
  paperId: string;
  paperCode: string;
  title: string;
  credits: string;
  attemptKind: string;
  internalMarks?: string | null;
  externalMarks?: string | null;
  graceMarks: string;
  totalMarks?: string | null;
  maxMarks: string;
  percent?: string | null;
  grade?: string | null;
  gradePoint?: string | null;
  passed: boolean;
  remark?: string | null;
}

export interface TermResult {
  id: string;
  sessionId: string;
  registrationId: string;
  studentId: string;
  studentNumber: string;
  displayName: string;
  creditsAttempted: string;
  creditsEarned: string;
  gradePoints: string;
  sgpa?: string | null;
  cgpa?: string | null;
  outcome: string;
  status: string;
  statusReason?: string | null;
  revision: number;
  supersedesId?: string | null;
  publishedAt?: string | null;
  notice?: string | null;
  lines: ResultLine[];
  version: number;
}

export interface ResultBatch {
  id: string;
  sessionId: string;
  status: string;
  statusReason?: string | null;
  runCount: number;
  candidates: number;
  computedCount: number;
  withheldCount: number;
  processInstanceId?: string | null;
  version: number;
}

export interface Publication {
  id: string;
  sessionId: string;
  status: string;
  statusReason?: string | null;
  scheduledOn?: string | null;
  publishedCount: number;
  skippedCount: number;
  lastError?: string | null;
  processInstanceId?: string | null;
  version: number;
}

export interface GradeCard {
  id: string;
  number: string;
  status: string;
  resultId: string;
  registrationId: string;
  issuedAt: string;
  result?: TermResult | null;
  version: number;
}

export interface Revaluation {
  id: string;
  number: string;
  kind: "revaluation" | "rechecking";
  sessionId: string;
  paperId: string;
  paperCode: string;
  studentNumber: string;
  displayName: string;
  status: string;
  feeAmount: string;
  feeChargeNumber?: string | null;
  feeSettled: boolean;
  outcome?: { fromMarks?: string | null; toMarks?: string | null; changed: boolean } | null;
  processInstanceId?: string | null;
  version: number;
}

export interface Eligibility {
  id: string;
  studentNumber: string;
  displayName: string;
  paperCode: string;
  attemptsUsed: number;
  maxAttempts: number;
  bestPercent?: string | null;
  bestGrade?: string | null;
  status: string;
  version: number;
}

export interface Closure {
  id: string;
  sessionId: string;
  status: string;
  checks: { blockers?: string[]; ready?: boolean };
  completedRegistrations: number;
  processInstanceId?: string | null;
  version: number;
}

export interface Completion {
  id: string;
  studentNumber: string;
  displayName: string;
  creditsRequired: string;
  creditsEarned: string;
  componentsRequired: number;
  componentsPassed: number;
  cgpa?: string | null;
  status: string;
  statusReason?: string | null;
  version: number;
}

export type BatchCommand = "compute" | "validate" | "finalise";
export type ResultCommand = "withhold" | "release" | "recompute" | "withdraw" | "restore";
export type PublicationCommand = "authorise" | "publish" | "resume" | "retry";
export type ReviewStep = "verify" | "start-review" | "verify-outcome" | "approve-marks";
export type Decision = "approve" | "reject";
