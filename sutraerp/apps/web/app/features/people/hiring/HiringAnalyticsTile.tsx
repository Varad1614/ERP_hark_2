/**
 * ERP_EXTENSIONS 5A: hiring analytics. Funnel per stage with stage-to-stage conversion, time to
 * hire, average days in each stage, sources and offer acceptance — institution-wide, or for one
 * requisition (`useRequisitionAnalytics`). Rates arrive as fractions (0–1) or `null`.
 */
import { BarChart3 } from "lucide-react";
import { BentoTile, DataTable, EmptyState, FormSelect, KeyValue, StatCells } from "@sutra/ui";
import { useState } from "react";
import { useHiringAnalytics, useRequisitionAnalytics, type OffersApi, defaultOffersApi } from "./offersApi";
import type { HiringAnalytics } from "./offersTypes";

const pct = (r: number | null | undefined) => (r == null ? "—" : `${Math.round(r * 1000) / 10}%`);
const days = (d: number | null | undefined) => (d == null ? "—" : `${Math.round(d * 10) / 10} d`);

const SINCE: { value: string; label: string }[] = [
  { value: "", label: "All time" },
  { value: "30", label: "Last 30 days" },
  { value: "90", label: "Last 90 days" },
  { value: "365", label: "Last 12 months" },
];

export function HiringAnalyticsTile({ institutionId, api = defaultOffersApi() }: { institutionId: string; api?: OffersApi }) {
  const [requisitionId, setRequisitionId] = useState("");
  const [since, setSince] = useState("");
  const sinceDays = since ? Number(since) : null;
  const all = useHiringAnalytics(institutionId, sinceDays, api);
  const one = useRequisitionAnalytics(institutionId, requisitionId, sinceDays, api);
  const data: HiringAnalytics | undefined = requisitionId ? one.data : all.data;
  const loading = requisitionId ? one.isLoading : all.isLoading;
  const reqs = all.data?.requisitions ?? [];
  const conv = new Map((data?.conversion ?? []).map((c) => [c.stage, c]));

  return (
    <BentoTile span={12} title="Hiring analytics" data-slice="5A" data-hiring-analytics>
      <div className="flex gap-2">
        <FormSelect label="Requisition" value={requisitionId} onChange={(e) => setRequisitionId(e.target.value)} data-field="analytics-requisition"
          options={[{ value: "", label: "All requisitions" }, ...reqs.map((r) => ({ value: r.requisitionId, label: r.requisitionNumber }))]} />
        <FormSelect label="Period" value={since} onChange={(e) => setSince(e.target.value)} data-field="analytics-since" options={SINCE} />
      </div>
      {!data ? (
        <EmptyState icon={BarChart3} title={loading ? "Loading analytics" : "No analytics yet"} compact>Figures appear once candidates move through the pipeline.</EmptyState>
      ) : (
        <>
          <StatCells columns={3} cells={[
            { label: "Candidates", value: data.candidates },
            { label: "Median time to hire", value: days(data.timeToHire.median), sub: `${data.timeToHire.count} hired` },
            { label: "Offer acceptance", value: pct(data.offers.acceptance), sub: `${data.offers.accepted} of ${data.offers.accepted + data.offers.declined} decided`, kind: data.offers.acceptance != null && data.offers.acceptance < 0.5 ? "attention" : undefined },
          ]} />
          <div data-analytics-funnel>
          <DataTable caption="Funnel" rows={data.funnel} rowKey={(s) => s.stage}
            empty={<EmptyState icon={BarChart3} title="No candidates" compact>Nothing in the pipeline for this period.</EmptyState>}
            columns={[
              { key: "s", header: "Stage", render: (s) => s.label },
              { key: "c", header: "Candidates", render: (s) => s.count },
              { key: "r", header: "Advanced to next", render: (s) => { const c = conv.get(s.stage); return c ? `${pct(c.rate)} (${c.advanced}/${c.reached})` : "—"; } },
            ]} />
          </div>
          <div data-analytics-stage-time>
          <DataTable caption="Time in stage" rows={data.averageDaysInStage} rowKey={(s) => s.stage}
            columns={[
              { key: "s", header: "Stage", render: (s) => data.funnel.find((f) => f.stage === s.stage)?.label ?? s.stage },
              { key: "a", header: "Average days", render: (s) => days(s.average) },
            ]} />
          </div>
          <div data-analytics-sources>
          <DataTable caption="Sources" rows={data.sources} rowKey={(s) => s.source}
            empty={<EmptyState icon={BarChart3} title="No sources" compact>Record a source on candidates to compare channels.</EmptyState>}
            columns={[
              { key: "s", header: "Source", render: (s) => s.source },
              { key: "t", header: "Candidates", render: (s) => s.total },
              { key: "h", header: "Hired", render: (s) => s.hired },
              { key: "r", header: "Hire rate", render: (s) => pct(s.total ? s.hired / s.total : null) },
            ]} />
          </div>
          <KeyValue label="Offers" value={`${data.offers.sent} sent · ${data.offers.accepted} accepted · ${data.offers.declined} declined · ${data.offers.expired} expired · ${data.offers.withdrawn} withdrawn · ${data.offers.inPlay} in play`} />
        </>
      )}
    </BentoTile>
  );
}
