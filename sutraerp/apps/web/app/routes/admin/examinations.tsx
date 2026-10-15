import { useParams } from "react-router";
import { MalpracticeAdminTab, MarksAdminTab } from "../../features/exams/marks";
import { ExamsFrame, ExamsOverview } from "../../features/exams/overview";
import { ResultsAdminTab, RevaluationAdminTab } from "../../features/exams/results";
import { RegistrationsAdminTab, ScheduleAdminTab, SessionsAdminTab } from "../../features/exams/setup";

export function meta() {
  return [{ title: "Examinations · SutraERP" }];
}

/**
 * Admin → Examinations (`/examinations/:tab?`). Re-skin v2 (R4): AreaHeader with Overview ·
 * Schedule · Marks · Results and the rest behind "More"; every former tab keeps its URL.
 */
export default function Examinations() {
  const { tab } = useParams();
  return (
    <ExamsFrame tab={tab}>
      {(institution, current) => {
        switch (current.key) {
          case "sessions":
            return <SessionsAdminTab institution={institution} />;
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
            return <ExamsOverview institution={institution} />;
        }
      }}
    </ExamsFrame>
  );
}
