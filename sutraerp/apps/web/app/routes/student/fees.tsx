import { AreaHeader, BentoGrid, BentoTile } from "@sutra/ui";
import { StudentFees, useFeesSummary } from "../../features/finance/fees";
import { useSession } from "../../lib/auth";

export function meta() {
  return [{ title: "Fees · SutraERP" }];
}

/**
 * Student Fees (`/fees`), re-skin v2 (R5): the prototype page (stu-fees.png) — header with the
 * student's name and roll number, Status / Next due / Paid tiles, Invoices with Pay now.
 */
export default function Fees() {
  const { role } = useSession();
  const summary = useFeesSummary(role === "student");
  if (role !== "student")
    return (
      <BentoGrid data-screen="fees">
        <BentoTile span={12} variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Fees is the students’ view; staff use Finance.</p>
        </BentoTile>
      </BentoGrid>
    );
  return (
    <BentoGrid data-screen="fees">
      <AreaHeader title="Fees" summary={summary ?? "Your invoices, payments and receipts"} data-page-head="fees" />
      <StudentFees />
    </BentoGrid>
  );
}
