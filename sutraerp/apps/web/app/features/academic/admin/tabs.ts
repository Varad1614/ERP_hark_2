/**
 * Re-skin v2 (R3) header tabs of the Academics area (≤5 visible; the rest fold into More).
 * `/academics`: Overview · Curriculum · Calendar · Delivery + More (Structure, Programmes, School
 * model). `/academic-delivery`: Offerings · Timetable · Attendance · Internal assessment + More
 * (Faculty assignment, Monitoring, Flexible learning, Credit recognition). Old URLs keep working.
 */
import type { FrameTab } from "../kit";

export const ACADEMICS_AREA_TABS: FrameTab[] = [
  { key: "overview", label: "Overview" },
  { key: "curriculum", label: "Curriculum" },
  { key: "calendar", label: "Calendar" },
  { key: "delivery", label: "Delivery", to: "/academic-delivery" },
  { key: "structure", label: "Academic structure" },
  { key: "programmes", label: "Programmes" },
  { key: "school-model", label: "School model" },
];

export const DELIVERY_AREA_TABS: FrameTab[] = [
  { key: "offerings", label: "Offerings" },
  { key: "timetable", label: "Timetable" },
  { key: "attendance", label: "Attendance" },
  { key: "assessment", label: "Internal assessment" },
  { key: "faculty", label: "Faculty assignment" },
  { key: "monitoring", label: "Monitoring" },
  { key: "flexible", label: "Flexible learning" },
  { key: "recognition", label: "Credit recognition" },
];

/** Known tab key for a route param (unknown / missing → the first tab). */
export function tabKey(tabs: FrameTab[], param: string | undefined): string {
  return tabs.find((t) => t.key === param && !t.to)?.key ?? tabs[0]!.key;
}
