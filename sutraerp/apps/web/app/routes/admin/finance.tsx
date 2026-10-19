import { useParams } from "react-router";
import { AdjustmentsAdminTab, AidAdminTab, ChargesAdminTab } from "../../features/finance/charges";
import { LedgerAdminTab, PeriodsAdminTab, TreasuryAdminTab } from "../../features/finance/ledger";
import { FinanceOverview } from "../../features/finance/overview";
import { CollectionsAdminTab, ReceivablesAdminTab, RefundsAdminTab } from "../../features/finance/payments";
import { FinanceFrame } from "../../features/finance/ui";

export function meta() {
  return [{ title: "Finance · SutraERP" }];
}

/**
 * Admin → Finance (`/finance/:tab?`). Re-skin v2 (R5): one area header (Overview / Collections /
 * Receivables / Ledger + More: Fees & charges, Adjustments, Financial aid, Refunds, Periods,
 * Bank & treasury); `/finance` is the prototype overview. Every Batch 5 tab keeps its route.
 */
export default function Finance() {
  const { tab } = useParams();
  return (
    <FinanceFrame tab={tab}>
      {(institution, current) => {
        switch (current) {
          case "charges":
            return <ChargesAdminTab institution={institution} />;
          case "adjustments":
            return <AdjustmentsAdminTab institution={institution} />;
          case "aid":
            return <AidAdminTab institution={institution} />;
          case "collections":
            return <CollectionsAdminTab institution={institution} />;
          case "refunds":
            return <RefundsAdminTab institution={institution} />;
          case "receivables":
            return <ReceivablesAdminTab institution={institution} />;
          case "ledger":
            return <LedgerAdminTab institution={institution} />;
          case "periods":
            return <PeriodsAdminTab institution={institution} />;
          case "treasury":
            return <TreasuryAdminTab institution={institution} />;
          default:
            return <FinanceOverview institution={institution} />;
        }
      }}
    </FinanceFrame>
  );
}
