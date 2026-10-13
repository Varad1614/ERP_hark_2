import { useParams } from "react-router";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { ACADEMIC_TABS } from "../../features/academic";
import { AssessmentAdminTab, AttendanceAdminTab, MonitoringAdminTab, TimetableAdminTab } from "../../features/academic/delivery";
import { CreditRecognitionAdminTab, FlexibleLearningAdminTab } from "../../features/academic/flexible";
import { FacultyAssignmentsAdminTab, OfferingsAdminTab } from "../../features/academic/offering";

export function meta() {
  return [{ title: "Academic delivery · SutraERP" }];
}

/**
 * Admin → Academic delivery (`/academic-delivery/:tab?`). Foundation frame: every tab is already
 * wired to its slice's component (docs/batch4-contracts.md §10), so slices never edit this file.
 */
export default function AcademicDelivery() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Academic delivery" base="/academic-delivery" tabs={ACADEMIC_TABS} tab={tab}>
      {(institution, current) => {
        switch (current.key) {
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
    </AdminScreen>
  );
}
