/**
 * Slice-6B wire types (marks, evaluation rounds, moderation, gradebook, malpractice). Decimals
 * (marks, percentages, grade points) are strings exactly as the API sends them — never parsed to
 * floats for display.
 */

export interface Items<T> {
  items: T[];
}

export interface MarkSheet {
  id: string;
  sessionId: string;
  paperId: string;
  status: string;
  statusReason?: string | null;
  dueOn?: string | null;
  secondEvaluationRequired: boolean;
  differenceThreshold: string;
  submittedBy?: string | null;
  submittedAt?: string | null;
  verifiedBy?: string | null;
  verifiedAt?: string | null;
  approvalRequestedAt?: string | null;
  awaitingDecision: boolean;
  approvedBy?: string | null;
  approvedAt?: string | null;
  finalisedAt?: string | null;
  processInstanceId?: string | null;
  version: number;
}

export interface MarkPreview {
  percent?: string | null;
  grade?: string | null;
  gradePoint?: string | null;
  passed: boolean;
}

export interface MarkRow {
  id: string;
  paperId: string;
  registrationId: string;
  registrationNumber: string;
  studentNumber: string;
  displayName: string;
  registrationStatus: string;
  paperStatus: string;
  attemptKind: string;
  attemptNo: number;
  internalMarks?: string | null;
  internalMax?: string | null;
  internalSourceId?: string | null;
  externalMarks?: string | null;
  externalMax: string;
  graceMarks: string;
  absent: boolean;
  status: string;
  effective: boolean;
  preview?: MarkPreview | null;
  version: number;
}

export interface MarkRowList {
  paperId: string;
  paperCode: string;
  paperStatus: string;
  sheet?: MarkSheet | null;
  items: MarkRow[];
}

export interface EntryRow {
  id: string;
  version: number;
  externalMarks?: string;
  absent?: boolean;
}

export interface BulkSummary {
  processed: number;
  skipped: number;
  failed: { registrationId: string; reason: string }[];
  missing: string[];
}

/** A paper with its sheet status (faculty `/me/assessments`, admin `/institutions/{id}/mark-sheets`). */
export interface Assessment {
  paperId: string;
  sessionId: string;
  sessionCode: string;
  sessionName: string;
  sessionStatus: string;
  paperCode: string;
  title: string;
  paperStatus: string;
  acceptsMarks: boolean;
  sheetId?: string | null;
  sheetStatus?: string | null;
  candidates: number;
  entered: number;
  sections: string[];
}

export interface GradebookLine {
  paperMarkId: string;
  registrationId: string;
  studentNumber: string;
  displayName: string;
  attemptKind: string;
  status: string;
  absent: boolean;
  internalMarks?: string | null;
  externalMarks?: string | null;
  graceMarks: string;
  percent?: string | null;
  grade?: string | null;
  gradePoint?: string | null;
  passed: boolean;
}

export interface Gradebook {
  paperId: string;
  paperCode: string;
  title: string;
  credits: string;
  passPercent: string;
  internalWeight: string;
  candidates: number;
  graded: number;
  passed: number;
  failed: number;
  absent: number;
  averagePercent?: string | null;
  distribution: { grade: string; count: number }[];
  lines: GradebookLine[];
}

export interface Moderation {
  id: string;
  sessionId: string;
  paperId: string;
  markSheetId: string;
  kind: string;
  value: string;
  reason: string;
  status: string;
  statusReason?: string | null;
  report: unknown;
  appliedCount: number;
  processInstanceId?: string | null;
  decidedBy?: string | null;
  createdAt: string;
  version: number;
}

export interface MalpracticeDecision {
  id: string;
  kind: string;
  outcome: string;
  penalty?: string | null;
  marksDeduction?: string | null;
  reason?: string | null;
  decidedBy?: string | null;
  createdAt: string;
}

export interface MalpracticeCase {
  id: string;
  caseNumber: string;
  sessionId: string;
  registrationId: string;
  paperId?: string | null;
  studentId: string;
  allegation: string;
  evidence?: string | null;
  status: string;
  statusReason?: string | null;
  penalty?: string | null;
  marksDeduction?: string | null;
  pending?: { kind: string; outcome?: string | null; penalty?: string | null; marksDeduction?: string | null; reason?: string | null } | null;
  mergedIntoId?: string | null;
  createdAt: string;
  decisions: MalpracticeDecision[];
  version: number;
}

export type SheetCommand = "submit" | "verify" | "request-approval" | "finalise";
export type CaseCommand = "investigate" | "hearing" | "appeal" | "reopen" | "close" | "archive";
