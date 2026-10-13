import { useParams } from "react-router";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { FINANCE_TABS } from "../../features/finance";
import { AdjustmentsAdminTab, AidAdminTab, ChargesAdminTab } from "../../features/finance/charges";
import { LedgerAdminTab, PeriodsAdminTab, TreasuryAdminTab } from "../../features/finance/ledger";
import { CollectionsAdminTab, ReceivablesAdminTab, RefundsAdminTab } from "../../features/finance/payments";

export function meta() {
  return [{ title: "Finance · SutraERP" }];
}

/**
 * Admin → Finance (`/finance/:tab?`). Foundation frame: every tab is already wired to its slice's
 * component (docs/batch5-contracts.md §10), so slices never edit this file.
 */
export default function Finance() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Finance" base="/finance" tabs={FINANCE_TABS} tab={tab}>
      {(institution, current) => {
        switch (current.key) {
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
            return <ChargesAdminTab institution={institution} />;
        }
      }}
    </AdminScreen>
  );
}
