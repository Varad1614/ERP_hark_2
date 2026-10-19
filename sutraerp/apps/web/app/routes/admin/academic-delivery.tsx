import { useParams } from "react-router";
import { useAcademicsData } from "../../features/academic/admin/overview";
import { DELIVERY_AREA_TABS, tabKey } from "../../features/academic/admin/tabs";
import { AssessmentAdminTab, AttendanceAdminTab, MonitoringAdminTab, TimetableAdminTab } from "../../features/academic/delivery";
import { CreditRecognitionAdminTab, FlexibleLearningAdminTab } from "../../features/academic/flexible";
import { AcademicFrame } from "../../features/academic/kit";
import { FacultyAssignmentsAdminTab, OfferingsAdminTab } from "../../features/academic/offering";
import { useInstitutions } from "../../features/institution/hooks";

export function meta() {
  return [{ title: "Academic delivery · SutraERP" }];
}

/**
 * Admin → Academic delivery (`/academic-delivery/:tab?`), re-skin v2 (R3): back to Academics,
 * Offerings · Timetable · Attendance · Internal assessment + More (faculty assignment, monitoring,
 * flexible learning, credit recognition).
 */
export default function AcademicDelivery() {
  const { tab } = useParams();
  const current = tabKey(DELIVERY_AREA_TABS, tab);
  const { current: inst } = useInstitutions();
  const data = useAcademicsData(inst?.id);
  const active = data.offerings.filter((o) => o.status === "active").length;
  const sections = data.offerings.reduce((n, o) => n + o.sections.filter((s) => s.status === "active" || s.status === "planned").length, 0);
  const line = data.loading ? "Loading course offerings…" : `${data.offerings.length} course ${data.offerings.length === 1 ? "offering" : "offerings"} · ${active} active · ${sections} ${sections === 1 ? "section" : "sections"}`;
  return (
    <AcademicFrame title="Academic delivery" base="/academic-delivery" back={{ label: "Academics", to: "/academics" }} tabs={DELIVERY_AREA_TABS} current={current} summary={() => line}>
      {(institution) => {
        switch (current) {
          case "faculty":
            return <FacultyAssignmentsAdminTab institution={institution} />;
          case "timetable":
            return <TimetableAdminTab institution={institution} />;
          case "attendance":
            return <AttendanceAdminTab institution={institution} />;
          case "monitoring":
            return <MonitoringAdminTab institution={institution} />;
          case "assessment":
            return <AssessmentAdminTab institution={institution} />;
          case "flexible":
            return <FlexibleLearningAdminTab institution={institution} />;
          case "recognition":
            return <CreditRecognitionAdminTab institution={institution} />;
          default:
            return <OfferingsAdminTab institution={institution} />;
        }
      }}
    </AcademicFrame>
  );
}
