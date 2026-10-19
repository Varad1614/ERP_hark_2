/**
 * ERP_EXTENSIONS 2C: stock demand and expiry (ported from Everything's inventory demand, reorder
 * and lots logic, MIT). Reorder shows units issued per day, days of cover and what to order,
 * most urgent first; suggested reorder levels are applied only when chosen. Expiry lists dated
 * stock that is past or near its date. Planning sets an item's lead time, pack size and expiry
 * tracking; Settings holds the institution's windows.
 */
import { CalendarClock } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, Toggle } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../../lib/api";
import { useCommand } from "../../../institution/hooks";
import { formatInr } from "../../../finance";
import { createHttpDemandApi, type DemandApi } from "./api";
import { attention, differs, qty, settingsProblem } from "./demand";
import { createMockDemandApi } from "./mock";
import type { DemandRow, DemandSettings } from "./types";

let fallback: DemandApi | null = null;
export function defaultDemandApi(): DemandApi {
  fallback ??= USES_API ? createHttpDemandApi() : createMockDemandApi();
  return fallback;
}

type Tab = "reorder" | "expiry" | "planning" | "settings";
const TABS: { value: Tab; label: string }[] = [
  { value: "reorder", label: "Reorder" },
  { value: "expiry", label: "Expiry" },
  { value: "planning", label: "Planning" },
  { value: "settings", label: "Settings" },
];

function statusBadge(r: DemandRow) {
  const why = attention(r);
  if (!why) return <Badge kind="done">In stock</Badge>;
  return <Badge kind={r.status === "out" || r.runningOut ? "attention" : "info"}>{why}</Badge>;
}

export function StockDemandTile({ institutionId, api = defaultDemandApi() }: { institutionId: string; api?: DemandApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("reorder");
  const [onlyAttention, setOnlyAttention] = useState(true);
  const [picked, setPicked] = useState<string[]>([]);
  const [plan, setPlan] = useState<{ itemId: string; lead: string; pack: string; safety: string; track: boolean }>({ itemId: "", lead: "", pack: "", safety: "", track: false });
  const [draft, setDraft] = useState<DemandSettings | null>(null);
  const key = (...p: string[]) => ["institution", "stock-demand", institutionId, ...p];
  const demand = useQuery({ queryKey: key("demand", String(onlyAttention)), queryFn: () => api.demand(institutionId, { attention: onlyAttention }), enabled: tab === "reorder" });
  const everything = useQuery({ queryKey: key("demand", "all"), queryFn: () => api.demand(institutionId), enabled: tab === "planning" });
  const expiry = useQuery({ queryKey: key("expiry"), queryFn: () => api.expiry(institutionId), enabled: tab === "expiry" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId), enabled: tab === "settings" });
  const lots = useQuery({ queryKey: key("lots", plan.itemId), queryFn: () => api.lots(plan.itemId), enabled: tab === "planning" && plan.itemId !== "" });
  const shown = draft ?? settings.data ?? null;
  const toggle = (id: string) => setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-demand-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );

  const pickItem = (id: string) => {
    const r = everything.data?.items.find((x) => x.itemId === id);
    setPlan({ itemId: id, lead: r?.leadTimeDays?.toString() ?? "", pack: r?.packSize ? String(Number(r.packSize)) : "", safety: "", track: r?.trackExpiry ?? false });
  };

  return (
    <BentoTile span={12} title="Stock demand & expiry" data-slice="2C" data-stock-demand action={tabs}>
      {tab === "reorder" && demand.data && (
        <>
          <KeyValue label="Measured over" value={demand.data.enough ? `${demand.data.measuredDays} of ${demand.data.windowDays} days` : `${demand.data.measuredDays} days of history: too little for rates yet`} />
          <Toggle label="Only items needing attention" checked={onlyAttention} onChange={(e) => setOnlyAttention(e.target.checked)} />
          <DataTable caption="Reorder suggestions" rows={demand.data.items} rowKey={(r) => r.itemId}
            empty={<EmptyState icon={CalendarClock} title="Nothing to reorder">Every item has enough cover.</EmptyState>}
            columns={[
              { key: "pick", header: "", render: (r) => (differs(r) ? <Checkbox label="" aria-label={`Pick ${r.itemCode}`} checked={picked.includes(r.itemId)} onChange={() => toggle(r.itemId)} /> : null) },
              { key: "item", header: "Item", render: (r) => <span data-item={r.itemCode}>{`${r.itemCode} · ${r.itemName}`}</span> },
              { key: "avail", header: "Available", render: (r) => `${qty(r.available)} ${r.unit}` },
              { key: "rate", header: "Demand", render: (r) => r.rateLabel || "—" },
              { key: "cover", header: "Cover", render: (r) => r.coverLabel || "—" },
              { key: "level", header: "Reorder level", render: (r) => (differs(r) ? `${qty(r.reorderLevel)} → ${qty(r.suggestedReorderLevel)}` : qty(r.reorderLevel)) },
              { key: "order", header: "Order now", render: (r) => (Number(r.suggestedOrderQuantity) > 0 ? `${qty(r.suggestedOrderQuantity)}${Number(r.onOrder) > 0 ? ` (+${qty(r.onOrder)} on order)` : ""}` : "—") },
              { key: "status", header: "Status", render: (r) => statusBadge(r) },
            ]} />
          <Button size="sm" data-action="apply-thresholds" disabled={busy || picked.length === 0} onClick={() => void run(async () => {
            const r = await api.applyThresholds(institutionId, picked);
            setPicked([]);
            return r;
          }, "Reorder levels updated")}>Use suggested reorder level ({picked.length})</Button>
        </>
      )}

      {tab === "expiry" && expiry.data && (
        <DataTable caption={`Expired or expiring within ${expiry.data.horizonDays} days`} rows={expiry.data.items} rowKey={(r) => r.itemId}
          empty={<EmptyState icon={CalendarClock} title="Nothing expiring">No dated stock is past or near its date.</EmptyState>}
          columns={[
            { key: "item", header: "Item", render: (r) => `${r.itemCode} · ${r.itemName}` },
            { key: "expired", header: "Expired", render: (r) => (Number(r.expired) > 0 ? <Badge kind="attention">{`${qty(r.expired)} ${r.unit}`}</Badge> : "—") },
            { key: "soon", header: "Expiring soon", render: (r) => (Number(r.soon) > 0 ? `${qty(r.soon)} ${r.unit}` : "—") },
            { key: "soonest", header: "Earliest date", render: (r) => r.soonest ?? "—" },
            { key: "value", header: "Value at cost", render: (r) => formatInr(r.value) },
          ]} />
      )}

      {tab === "planning" && (
        <>
          <FormSelect label="Item" value={plan.itemId} onChange={(e) => pickItem(e.target.value)}
            options={[{ value: "", label: "Choose an item" }, ...(everything.data?.items ?? []).map((r) => ({ value: r.itemId, label: `${r.itemCode} · ${r.itemName}` }))]} />
          {plan.itemId && (
            <>
              <FormGrid columns={3}>
                <TextField label="Supplier lead time (days)" inputMode="numeric" value={plan.lead} onChange={(e) => setPlan({ ...plan, lead: e.target.value })} />
                <TextField label="Pack size" inputMode="decimal" value={plan.pack} onChange={(e) => setPlan({ ...plan, pack: e.target.value })} />
                <TextField label="Safety days (blank = default)" inputMode="numeric" value={plan.safety} onChange={(e) => setPlan({ ...plan, safety: e.target.value })} />
              </FormGrid>
              <Toggle label="Track expiry dates" checked={plan.track} onChange={(e) => setPlan({ ...plan, track: e.target.checked })} />
              <Button size="sm" data-action="save-planning" disabled={busy} onClick={() => void run(() => api.savePlanning(plan.itemId, {
                leadTimeDays: plan.lead.trim() ? Number(plan.lead) : null,
                packSize: plan.pack.trim() ? plan.pack.trim() : null,
                safetyDays: plan.safety.trim() ? Number(plan.safety) : null,
                trackExpiry: plan.track,
              }), "Planning saved")}>Save planning</Button>
              {lots.data?.trackExpiry && (
                <DataTable caption="Lots, first to expire first" rows={lots.data.items} rowKey={(l) => `${l.locationId}|${l.expiryOn ?? ""}`}
                  empty={<EmptyState icon={CalendarClock} title="No lots" compact>Receipts with an expiry date add lots.</EmptyState>}
                  columns={[
                    { key: "loc", header: "Location", render: (l) => l.locationCode },
                    { key: "date", header: "Expires", render: (l) => l.expiryOn ?? "No date" },
                    { key: "qty", header: "Quantity", render: (l) => qty(l.quantity) },
                  ]} />
              )}
            </>
          )}
        </>
      )}

      {tab === "settings" && shown && (
        <>
          <FormGrid columns={2}>
            <TextField label="Demand window (days)" inputMode="numeric" value={String(shown.demandDays)} onChange={(e) => setDraft({ ...shown, demandDays: Number(e.target.value) })} />
            <TextField label="Safety days" inputMode="numeric" value={String(shown.safetyDays)} onChange={(e) => setDraft({ ...shown, safetyDays: Number(e.target.value) })} />
            <TextField label="Order enough for (days, 0 = twice the reorder level)" inputMode="numeric" value={String(shown.coverageDays)} onChange={(e) => setDraft({ ...shown, coverageDays: Number(e.target.value) })} />
            <TextField label="Expiring-soon horizon (days)" inputMode="numeric" value={String(shown.expiryDays)} onChange={(e) => setDraft({ ...shown, expiryDays: Number(e.target.value) })} />
          </FormGrid>
          {draft && settingsProblem(draft) && <p role="alert">{settingsProblem(draft)}</p>}
          <Button size="sm" data-action="save-demand-settings" disabled={busy || !draft || settingsProblem(draft) !== null} onClick={() => void run(async () => {
            const s = await api.saveSettings(institutionId, draft!);
            setDraft(null);
            return s;
          }, "Settings saved")}>Save settings</Button>
        </>
      )}
      {dialog}
    </BentoTile>
  );
}
