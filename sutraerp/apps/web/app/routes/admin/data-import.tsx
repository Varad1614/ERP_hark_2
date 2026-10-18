import { useParams } from "react-router";
import { useSession } from "../../lib/auth";
import { AreaFrame, type AreaGroupTab } from "../../features/campus/AreaFrame";
import { DataImportTab, IMPORT_TABS, importTabsFor, type ImportTabKey } from "../../features/imports";

export function meta() {
  return [{ title: "Data import · SutraERP" }];
}

const SUMMARY: Record<ImportTabKey, string> = {
  import: "Bring records in from CSV or Excel · template, mapping, dry run, commit",
  approvals: "Imports waiting for a second person’s approval",
  history: "Every import of this institution, newest first",
  export: "Download records in their import columns",
};

/**
 * Admin → Data import (`/data-import/:tab?`), Part 2 · S4-ui-import. The tabs a session sees follow
 * its permissions (`platform.import.manage` → Import / History / Export, `platform.import.approve`
 * → Approvals); the API enforces both on every request.
 */
export default function DataImport() {
  const { tab } = useParams();
  const session = useSession();
  const allowed = importTabsFor(session);
  const tabs: AreaGroupTab[] = IMPORT_TABS.filter((t) => allowed.includes(t.key)).map((t) => ({
    id: t.key,
    label: t.label,
    to: t.key === "import" ? "/data-import" : `/data-import/${t.key}`,
    match: t.key === "import" ? ["/data-import/import"] : t.key === allowed[0] ? ["/data-import"] : [],
  }));
  const requested = IMPORT_TABS.find((t) => t.key === tab)?.key;
  const current: ImportTabKey = requested ?? allowed[0] ?? "import";
  return (
    <AreaFrame screen="data-import" title="Data import" summary={SUMMARY[current]} tabs={tabs} activeTab={current}>
      {(institution) => <DataImportTab tab={current} institution={institution} />}
    </AreaFrame>
  );
}
