/**
 * Workspace directory → screens (UI re-skin). A workspace is "live" for a role when a built screen
 * serves it; the rest are "coming" with the batch that delivers their domain (docs/batches.md).
 * Every workspace keeps its catalogue page (`/workspaces/:key`) either way.
 */
import type { Role } from "./roles";

type Routes = Partial<Record<Role, string>>;
const all = (to: string): Routes => ({ administrator: to, faculty: to, student: to });

/** Built screens per workspace key and role (Batches 1–9). */
export const WORKSPACE_SCREENS: Record<string, Routes> = {
  // A · Platform (Batch 1)
  "W-A02": all("/profile"),
  "W-A03": all("/account/security"),
  "W-A05": all("/todos"),
  "W-H02": all("/notifications"),
  // B · Institution (Batch 2)
  "W-B01": { administrator: "/campus/institution" },
  "W-B02": { administrator: "/campus/organisation" },
  "W-B03": { administrator: "/campus/campuses" },
  "W-B04": { administrator: "/academics/curriculum" },
  "W-B05": { administrator: "/academics/calendar" },
  // C · Student lifecycle (Batch 3)
  "W-C01": { administrator: "/students/admissions" },
  "W-C02": { administrator: "/students/directory", student: "/documents" },
  "W-C03": { administrator: "/students/registration", student: "/registration" },
  "W-C04": { administrator: "/students/progression" },
  "W-C05": { administrator: "/students/exits" },
  "W-C06": { administrator: "/students/guardians" },
  "W-C07": { administrator: "/students/scholarships" },
  // D · Academic (Batch 4)
  "W-D01": { administrator: "/academic-delivery/offerings", faculty: "/classes", student: "/courses" },
  "W-D02": { administrator: "/academic-delivery/faculty", faculty: "/classes" },
  "W-D03": { administrator: "/academic-delivery/timetable", faculty: "/timetable", student: "/timetable" },
  "W-D04": { administrator: "/academic-delivery/attendance", faculty: "/attendance" },
  "W-D05": { administrator: "/academic-delivery/monitoring", faculty: "/my-students" },
  "W-D06": { administrator: "/academic-delivery/flexible", faculty: "/resources", student: "/library" },
  "W-D07": { administrator: "/academic-delivery/assessment", student: "/assignments" },
  // F · Finance (Batch 5)
  "W-F00": { administrator: "/finance/charges" },
  "W-F01": { administrator: "/finance/charges", student: "/fees" },
  "W-F02": { administrator: "/finance/collections", student: "/fees" },
  "W-F03": { administrator: "/finance/refunds" },
  "W-F04": { administrator: "/finance/adjustments" },
  "W-F05": { administrator: "/finance/ledger" },
  "W-F07": { administrator: "/finance/receivables" },
  "W-F09": { administrator: "/finance/periods" },
  "W-F10": { administrator: "/finance/treasury" },
  "W-F11": { administrator: "/finance/aid" },
  // I · Examinations (Batch 6)
  "W-I01": { administrator: "/examinations/sessions" },
  "W-I02": { administrator: "/examinations/sessions" },
  "W-I03": { administrator: "/examinations/schedule", student: "/results" },
  "W-I04": { administrator: "/examinations/registrations", student: "/results" },
  "W-I05": { administrator: "/examinations/registrations", student: "/results" },
  "W-I06": { administrator: "/examinations/conduct" },
  "W-I07": { administrator: "/examinations/conduct" },
  "W-I08": { administrator: "/examinations/marks", faculty: "/assessments" },
  "W-I09": { administrator: "/examinations/results" },
  "W-I10": { administrator: "/examinations/results", student: "/results" },
  "W-I11": { administrator: "/examinations/revaluation", student: "/results" },
  "W-I12": { administrator: "/examinations/revaluation" },
  "W-I13": { administrator: "/examinations/revaluation", student: "/results" },
  "W-I14": { administrator: "/examinations/malpractice" },
  "W-I16": { administrator: "/examinations/revaluation" },
  // E · People & workforce (Batch 7)
  "W-E01": { administrator: "/people/staff", faculty: "/leave" },
  "W-E02": { administrator: "/people/positions" },
  "W-E03": { administrator: "/people/staff" },
  "W-E04": { administrator: "/people/recruitment", faculty: "/leave" },
  "W-E05": { administrator: "/people/leave", faculty: "/leave" },
  "W-E06": { administrator: "/people/payroll", faculty: "/leave" },
  "W-E07": { administrator: "/people/staff", faculty: "/leave" },
  // G · H · J Campus · Services · Resources (Batch 8)
  "W-G01": { administrator: "/campus/facilities" },
  "W-G02": { administrator: "/campus/labs" },
  "W-G03": { administrator: "/campus/hostel" },
  "W-G04": { administrator: "/campus/transport" },
  "W-G05": { administrator: "/campus/library", student: "/library" },
  "W-G10": { administrator: "/campus/infrastructure" },
  "W-H07": { administrator: "/reports/dashboards" },
  "W-H08": { administrator: "/reports/regulatory" },
  "W-J01": { administrator: "/campus/procurement" },
  "W-J02": { administrator: "/campus/procurement" },
  "W-J03": { administrator: "/campus/procurement" },
  "W-J04": { administrator: "/campus/procurement" },
  "W-J05": { administrator: "/campus/procurement" },
  "W-J06": { administrator: "/campus/inventory" },
  "W-J07": { administrator: "/campus/inventory" },
  "W-J08": { administrator: "/campus/assets" },
  // G00 governance · H09 accreditation (Batch 9 carry-overs)
  "W-G00": { administrator: "/campus/governance" },
  "W-H09": { administrator: "/reports/accreditation" },
};

/** Delivery batch by domain letter of the workspace key (W-<letter>NN). */
export function workspaceBatch(key: string): number {
  if (key === "W-H09" || key === "W-G00") return 9;
  const letter = key.charAt(2);
  return ({ A: 1, B: 2, C: 3, D: 4, F: 5, I: 6, E: 7, G: 8, H: 8, J: 8 } as Record<string, number>)[letter] ?? 9;
}

export function workspaceScreen(key: string, role: Role): string | null {
  return WORKSPACE_SCREENS[key]?.[role] ?? null;
}
