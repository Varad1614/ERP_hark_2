import { useParams } from "react-router";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { EXAM_TABS } from "../../features/exams";
import { MalpracticeAdminTab, MarksAdminTab } from "../../features/exams/marks";
import { ResultsAdminTab, RevaluationAdminTab } from "../../features/exams/results";
import { RegistrationsAdminTab, ScheduleAdminTab, SessionsAdminTab } from "../../features/exams/setup";

export function meta() {
  return [{ title: "Examinations · SutraERP" }];
}

/**
 * Admin → Examinations (`/examinations/:tab?`). Foundation frame: every tab is already wired to
 * its slice's component (docs/batch6-contracts.md §10), so slices never edit this file.
 */
export default function Examinations() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Examinations" base="/examinations" tabs={EXAM_TABS} tab={tab}>
      {(institution, current) => {
        switch (current.key) {
          case "schedule":
            return <ScheduleAdminTab institution={institution} />;
          case "registrations":
            return <RegistrationsAdminTab institution={institution} />;
          case "marks":
            return <MarksAdminTab institution={institution} />;
          case "malpractice":
            return <MalpracticeAdminTab institution={institution} />;
          case "results":
            return <ResultsAdminTab institution={institution} />;
          case "revaluation":
            return <RevaluationAdminTab institution={institution} />;
          default:
            return <SessionsAdminTab institution={institution} />;
        }
      }}
    </AdminScreen>
  );
}
