/**
 * Slice-4B wire types (timetable, sessions + attendance, monitoring, internal assessment). They
 * mirror the API DTOs (camelCase members, snake_case statuses); the API stays authoritative.
 */
export interface Items<T> {
  items: T[];
}

export interface TeachingSection {
  sectionId: string;
  offeringId: string;
  academicTermId: string;
  offeringCode: string;
  title: string;
  sectionCode: string;
  status: string;
  offeringStatus: string;
}

export interface SchedulingRequirement {
  id: string;
  institutionId: string;
  academicTermId: string;
  offeringId: string;
  sectionId: string;
  sessionsPerWeek: number;
  sessionMinutes: number;
  preferredDays: number[];
  roomLabel?: string | null;
  status: string;
  version: number;
}

export interface CreateRequirement {
  sectionId: string;
  sessionsPerWeek: number;
  sessionMinutes: number;
  preferredDays?: number[];
  roomLabel?: string | null;
}

export interface Conflict {
  code: string;
  severity: string;
  message: string;
  slotIds: string[];
}

export interface TimetableSlot {
  id: string;
  timetableId: string;
  sectionId: string;
  offeringId: string;
  offeringCode: string;
  title: string;
  sectionCode: string;
  dayOfWeek: number;
  startsAt: string;
  endsAt: string;
  roomLabel?: string | null;
  facultyNames: string[];
}

export type TimetableStatus = "planning" | "draft" | "validated" | "approved" | "published" | "active" | "frozen" | "revised" | "retired" | "archived";

export interface Timetable {
  id: string;
  institutionId: string;
  academicTermId: string;
  name: string;
  versionNo: number;
  revisionOfId?: string | null;
  status: TimetableStatus;
  statusReason?: string | null;
  validationReport?: { valid: boolean; conflicts: Conflict[]; validatedAt: string } | null;
  pendingDecision: boolean;
  version: number;
  slots: TimetableSlot[];
}

export type TimetableCommand = "generate" | "validate" | "submit" | "publish" | "activate" | "freeze" | "withdraw" | "retire" | "archive";

export interface WeeklyTimetable {
  items: TimetableSlot[];
  weekOf: string;
}

export type AttendanceMark = "present" | "absent" | "late" | "excused";

export interface AttendanceRecord {
  id?: string | null;
  studentId: string;
  studentNumber: string;
  displayName: string;
  mark?: AttendanceMark | null;
  remarks?: string | null;
  seated: boolean;
}

export type SessionStatus = "scheduled" | "started" | "in_progress" | "paused" | "completed" | "cancelled" | "abandoned" | "locked" | "archived";
export type CaptureStatus = "not_started" | "capturing" | "submitted" | "finalised";

export interface TeachingSession {
  id: string;
  sectionId: string;
  offeringId: string;
  sessionDate: string;
  startsAt: string;
  endsAt: string;
  topic?: string | null;
  status: SessionStatus;
  attendanceStatus: CaptureStatus;
  version: number;
  records: AttendanceRecord[];
}

export type SessionCommand = "start" | "pause" | "resume" | "complete" | "cancel" | "lock";

/** Governed correction of a finalised register (DEF-D-002/003): requested by faculty, decided by an approver other than the requester. */
export type CorrectionStatus = "requested" | "validated" | "approved" | "applied" | "rejected" | "withdrawn";

export interface AttendanceCorrection {
  id: string;
  sessionId: string;
  recordId: string;
  studentId: string;
  fromMark: AttendanceMark;
  toMark: AttendanceMark;
  reason: string;
  evidence?: string | null;
  status: CorrectionStatus;
  statusReason?: string | null;
  requestedBy?: string | null;
  decidedBy?: string | null;
  decidedAt?: string | null;
  version: number;
  createdAt: string;
}

/** A correction in the institution decision queue, with its student and session. */
export interface AttendanceCorrectionQueueItem extends AttendanceCorrection {
  studentNumber: string;
  displayName: string;
  sectionId: string;
  offeringCode: string;
  sectionCode: string;
  sessionDate: string;
  startsAt: string;
}

export interface RequestCorrection {
  recordId: string;
  toMark: AttendanceMark;
  reason: string;
  evidence?: string | null;
}

export interface CreateSession {
  sessionDate: string;
  startsAt?: string;
  endsAt?: string;
  timetableSlotId?: string;
  topic?: string | null;
}

export interface TaughtSection {
  sectionId: string;
  offeringId: string;
  offeringCode: string;
  title: string;
  sectionCode: string;
  seatedCount: number;
  sessions: TeachingSession[];
}

export interface AttendancePolicy {
  id: string;
  institutionId: string;
  name: string;
  thresholdPercent: number;
  minSessions: number;
  lateCountsAsPresent: boolean;
  excludeExcused: boolean;
  status: string;
  version: number;
}

export interface CreatePolicy {
  name: string;
  thresholdPercent: number;
  minSessions?: number;
}

export interface AttendanceSummaryLine {
  studentId: string;
  studentNumber: string;
  displayName: string;
  attended: number;
  total: number;
  percent: number;
  belowThreshold: boolean;
}

export interface AttendanceSummary {
  sectionId: string;
  thresholdPercent: number;
  policyId?: string | null;
  finalisedSessions: number;
  items: AttendanceSummaryLine[];
}

export interface MyAttendanceLine {
  sectionId: string;
  offeringCode: string;
  title: string;
  sectionCode: string;
  attended: number;
  total: number;
  percent: number;
  belowThreshold: boolean;
  thresholdPercent: number;
}

/** Server-computed overall attendance (one definition; `percent` null until a session counts). */
export interface AttendanceOverall {
  attended: number;
  total: number;
  percent: number | null;
}

export interface MyAttendance {
  items: MyAttendanceLine[];
  overall: AttendanceOverall;
}

export interface RiskAssessment {
  id: string;
  category: string;
  level: string;
  basis: string;
  rationale?: string | null;
  createdAt: string;
}

export interface MonitoringIntervention {
  id: string;
  title: string;
  ownerSubject?: string | null;
  status: string;
  version: number;
}

export interface MonitoringCase {
  id: string;
  institutionId: string;
  scopeKind: string;
  sectionId?: string | null;
  studentId?: string | null;
  title: string;
  ownerSubject: string;
  reviewerSubject?: string | null;
  status: string;
  pendingDecision?: string | null;
  outcomeSummary?: string | null;
  outcomePublishedAt?: string | null;
  cycleNo: number;
  reopenedAsId?: string | null;
  version: number;
  riskAssessments: RiskAssessment[];
  interventions: MonitoringIntervention[];
}

export interface OpenCase {
  scopeKind: "section";
  sectionId: string;
  title: string;
  reviewerSubject?: string | null;
}

export type InterventionCommand = "assign" | "trigger" | "start" | "complete" | "cancel";

export interface BlueprintComponent {
  key: string;
  title: string;
  kind: string;
  weight: number;
}

export interface AssessmentBlueprint {
  id: string;
  sectionId: string;
  title: string;
  totalMarks: number;
  components: BlueprintComponent[];
  status: string;
  version: number;
}

export interface AssessmentWindow {
  id: string;
  sectionId: string;
  blueprintId: string;
  componentKey: string;
  title: string;
  kind: string;
  opensAt: string;
  dueAt: string;
  extendedDueAt?: string | null;
  maxMarks: number;
  status: string;
  version: number;
}

export interface CreateWindow {
  blueprintId: string;
  componentKey: string;
  title: string;
  opensAt: string;
  dueAt: string;
  maxMarks: number;
  instructions?: string | null;
}

export interface InternalAssessmentResult {
  id: string;
  studentId: string;
  studentNumber: string;
  displayName: string;
  marksObtained: number;
  maxMarks: number;
  status: string;
  resultVersion: number;
  version: number;
}

export interface MyAssignment {
  id: string;
  sectionId: string;
  offeringCode: string;
  courseTitle: string;
  title: string;
  kind: string;
  instructions?: string | null;
  opensAt: string;
  dueAt: string;
  extendedDueAt?: string | null;
  maxMarks: number;
  status: string;
  submission?: { status: string; submittedAt?: string | null; marks?: number | null; feedback?: string | null } | null;
}
