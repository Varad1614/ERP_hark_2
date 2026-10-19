/** Slice-6A wire types (camelCase JSON of the `exam-setup`, `exam-scheduling`, `exam-registration` tags). Decimals are strings. */

export interface Items<T> {
  items: T[];
}

export interface ExamPolicy {
  id: string;
  institutionId: string;
  code: string;
  name: string;
  versionNo: number;
  supersedesId?: string | null;
  status: string;
  statusReason?: string | null;
  effectiveFrom?: string | null;
  minAttendancePercent?: string | null;
  requireDuesCleared: boolean;
  lateRegistrationAllowed: boolean;
  lateFeeCode?: string | null;
  lateFeeAmount?: string | null;
  maxAttempts: number;
  invigilatorRatio: number;
  version: number;
}

export interface ExamPaper {
  id: string;
  sessionId: string;
  offeringId?: string | null;
  paperCode: string;
  title: string;
  credits: string;
  mode: string;
  maxMarks: string;
  passPercent: string;
  internalWeight: string;
  durationMinutes?: number | null;
  status: string;
  version: number;
}

export interface ExamSession {
  id: string;
  institutionId: string;
  academicTermId: string;
  code: string;
  name: string;
  kind: string;
  status: string;
  policyId?: string | null;
  gradingSchemeId?: string | null;
  startsOn?: string | null;
  endsOn?: string | null;
  registrationOpensOn?: string | null;
  registrationClosesOn?: string | null;
  mergedIntoId?: string | null;
  revisions: number;
  papers: ExamPaper[];
  version: number;
}

export interface VenueAllocation {
  id: string;
  sittingId: string;
  venueId: string;
  venueCode: string;
  venueName: string;
  capacity: number;
  seats: number;
  status: string;
}

export interface ExamSitting {
  id: string;
  scheduleId: string;
  sessionId: string;
  paperId: string;
  paperCode: string;
  title: string;
  mode: string;
  sitsOn: string;
  startsAt: string;
  durationMinutes: number;
  status: string;
  statusReason?: string | null;
  postponedFrom?: string | null;
  venues: VenueAllocation[];
  version: number;
}

export interface ScheduleConflict {
  kind: string;
  sittingIds: string[];
  count: number;
  detail: string;
}

export interface ExamSchedule {
  id: string;
  sessionId: string;
  name: string;
  versionNo: number;
  supersedesId?: string | null;
  status: string;
  statusReason?: string | null;
  publishedAt?: string | null;
  sittings: ExamSitting[];
  conflicts: ScheduleConflict[];
  version: number;
}

export interface ExamVenue {
  id: string;
  code: string;
  name: string;
  capacity: number;
  location?: string | null;
  status: string;
  version: number;
}

export interface RegistrationPaper {
  registrationPaperId: string;
  paperId: string;
  paperCode: string;
  title: string;
  attemptKind: string;
  attemptNo: number;
  status: string;
}

export interface ExamRegistration {
  id: string;
  registrationNumber: string;
  sessionId: string;
  studentId: string;
  studentNumber: string;
  displayName: string;
  channel: string;
  status: string;
  statusReason?: string | null;
  papers: RegistrationPaper[];
  hallTicket?: { id: string; number: string; status: string } | null;
  arrangements: { id: string; kind: string; extraMinutes?: number | null; note: string; status: string }[];
  lateFee?: { feeCode: string; amount: string; chargeId: string; chargeNumber: string } | null;
  version: number;
}

export interface HallTicket {
  id: string;
  sessionId: string;
  registrationId: string;
  studentId: string;
  number: string;
  status: string;
  withheldReason?: string | null;
  specialArrangements?: string | null;
  replacesId?: string | null;
  issuedAt?: string | null;
  version: number;
}

export interface BulkSummary {
  processed: number;
  skipped: number;
  failed: { id?: string | null; reason: string }[];
}

export interface TimetableEntry {
  sessionId: string;
  registrationId: string;
  sittingId: string;
  paperId: string;
  paperCode: string;
  title: string;
  sitsOn: string;
  startsAt: string;
  durationMinutes: number;
  status: string;
  postponedFrom?: string | null;
  venueCode?: string | null;
  seatNo?: string | null;
}

export interface SelfOption {
  sessionId: string;
  code: string;
  name: string;
  registrationOpensOn?: string | null;
  registrationClosesOn?: string | null;
  window: string;
  registered: boolean;
  papers: { paperId: string; paperCode: string; title: string; credits: string; covered: boolean }[];
}

export interface MyHallTicket {
  registrationNumber: string;
  displayName: string;
  studentNumber: string;
  ticket: HallTicket;
}
