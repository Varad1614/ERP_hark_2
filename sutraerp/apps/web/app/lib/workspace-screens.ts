/**
 * Workspace directory → screens (UI re-skin). A workspace is "live" for a role when a built screen
 * serves it; the rest are "coming" with the batch that delivers their domain (docs/batches.md).
 * Every workspace keeps its catalogue page (`/workspaces/:key`) either way.
 */
import type { Role } from "./roles";

type Routes = Partial<Record<Role, string>>;
const all = (to: string): Routes => ({ administrator: to, faculty: to, student: to });

/** Built screens per workspace key and role (Batches 1–5). */
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
};

/** Delivery batch by domain letter of the workspace key (W-<letter>NN). */
export function workspaceBatch(key: string): number {
  if (/^W-H0[789]$/.test(key)) return 9;
  const letter = key.charAt(2);
  return ({ A: 1, B: 2, C: 3, D: 4, F: 5, I: 6, E: 7, G: 8, H: 8, J: 8 } as Record<string, number>)[letter] ?? 9;
}

export function workspaceScreen(key: string, role: Role): string | null {
  return WORKSPACE_SCREENS[key]?.[role] ?? null;
}
