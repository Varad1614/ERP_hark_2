/**
 * Conduct-slice wire types (camelCase JSON of the `exam-scheduling` tag): conduct records with
 * their attendance, invigilation staffing and duties, seating plans and exception cases.
 */

export interface ConductAttendance {
  registrationId: string;
  studentId: string;
  studentNumber: string;
  displayName: string;
  /** `present | absent | denied_entry`. */
  outcome: string;
  reason?: string | null;
}

export interface ConductRecord {
  id: string;
  sessionId: string;
  sittingId: string;
  paperId: string;
  /** `scheduled | in_progress | suspended | completed | postponed | terminated`. */
  status: string;
  statusReason?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
  presentCount: number;
  absentCount: number;
  deniedCount: number;
  pendingPostpone?: { newDate?: string; newStartsAt?: string; reason?: string } | null;
  processInstanceId?: string | null;
  attendance: ConductAttendance[];
  version: number;
}

export interface AttendanceEntry {
  registrationId: string;
  outcome: "present" | "absent" | "denied_entry";
  reason?: string;
}

export interface InvigilationDuty {
  id: string;
  institutionId: string;
  sittingId: string;
  venueId?: string | null;
  subjectRef: string;
  name?: string | null;
  /** `invigilator | chief_invigilator | practical_examiner | external_examiner`. */
  role: string;
  replacesId?: string | null;
  /** `assigned | confirmed | replaced | released | completed`. */
  status: string;
  statusReason?: string | null;
  version: number;
}

export interface InvigilationStaffing {
  sittingId: string;
  candidates: number;
  ratio: number;
  required: number;
  assigned: number;
  examiners: number;
  shortage: number;
  duties: InvigilationDuty[];
}

export interface SeatView {
  registrationId: string;
  studentId: string;
  studentNumber: string;
  displayName: string;
  venueId: string;
  venueCode: string;
  seatNo: string;
}

export interface SeatingPlan {
  id: string;
  sittingId: string;
  planNo: number;
  supersedesId?: string | null;
  candidates: number;
  /** `draft | published | revised | cancelled`. */
  status: string;
  statusReason?: string | null;
  publishedAt?: string | null;
  seats: SeatView[];
  version: number;
}

export interface ExamException {
  id: string;
  institutionId: string;
  sessionId: string;
  number: string;
  sittingId?: string | null;
  paperId?: string | null;
  registrationId?: string | null;
  /** `conduct | venue | paper | candidate | technical | other`. */
  category: string;
  classification?: string | null;
  /** `low | medium | high | critical`. */
  severity?: string | null;
  description: string;
  resolution?: string | null;
  /** `raised | classified | under_review | resolved | closed`. */
  status: string;
  statusReason?: string | null;
  version: number;
}

export interface ExceptionInput {
  category: string;
  description: string;
  sittingId?: string;
  paperId?: string;
  registrationId?: string;
  severity?: string;
}

/** Everything the Conduct screen shows for one sitting (`null` = not created yet). */
export interface SittingBoard {
  conduct: ConductRecord | null;
  seating: SeatingPlan | null;
  staffing: InvigilationStaffing | null;
}

export interface Items<T> {
  items: T[];
}
