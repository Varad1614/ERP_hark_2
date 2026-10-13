import { RoleScreen } from "../../features/academic/RoleScreen";
import { StudentChargesPanel } from "../../features/finance/charges";
import { StudentPaymentsPanel } from "../../features/finance/payments";

export function meta() {
  return [{ title: "Fees · SutraERP" }];
}

/**
 * Student Fees (`/fees`). Foundation frame (docs/batch5-contracts.md §10): 5A renders the
 * charges / instalments / concessions panel, 5B the pay + receipts panel; neither edits this file.
 */
export default function Fees() {
  return (
    <RoleScreen screen="fees" roles={["student"]} title="Fees" note="is the students’ view; staff use Finance.">
      {() => (
        <>
          <StudentChargesPanel />
          <StudentPaymentsPanel />
        </>
      )}
    </RoleScreen>
  );
}
