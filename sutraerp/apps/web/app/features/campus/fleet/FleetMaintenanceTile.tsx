/**
 * ERP_EXTENSIONS 2D: fleet maintenance (ported from Everything's fleet reminders, fuel and cost
 * logic, MIT). The board lists service reminders and renewals that are overdue, due soon or
 * snoozed; a vehicle's file records services (completing reminders), fill-ups, meter readings
 * and expenses; the cost report shows fuel, service and other spend with ₹ per km.
 */
import { Wrench } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, Toggle } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import { formatInr } from "../../finance";
import { createHttpFleetApi, type FleetApi } from "./api";
import { dueLine, RENEWAL_TYPES, STATUS_LABEL, UNITS, wholeOrNull } from "./fleet";
import { createMockFleetApi } from "./mock";
import type { DueStatus, FleetSettings } from "./types";

let fallback: FleetApi | null = null;
export function defaultFleetApi(): FleetApi {
  fallback ??= USES_API ? createHttpFleetApi() : createMockFleetApi();
  return fallback;
}

type Tab = "board" | "vehicle" | "costs" | "settings";
const TABS: { value: Tab; label: string }[] = [
  { value: "board", label: "Due" },
  { value: "vehicle", label: "Vehicle file" },
  { value: "costs", label: "Costs" },
  { value: "settings", label: "Settings" },
];

function badge(s: DueStatus) {
  return <Badge kind={s === "overdue" ? "attention" : s === "due_soon" ? "info" : "done"}>{STATUS_LABEL[s]}</Badge>;
}

export function FleetMaintenanceTile({ institutionId, api = defaultFleetApi() }: { institutionId: string; api?: FleetApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("board");
  const [vehicleId, setVehicleId] = useState("");
  const [svc, setSvc] = useState({ description: "", meter: "", amount: "", vendor: "", reminderIds: [] as string[] });
  const [fill, setFill] = useState({ meter: "", volume: "", price: "", partial: false });
  const [rem, setRem] = useState({ task: "", every: "", unit: "months", km: "" });
  const [ren, setRen] = useState({ type: "insurance", due: "", every: "1", unit: "years" });
  const [meter, setMeter] = useState("");
  const [warnings, setWarnings] = useState<string[]>([]);
  const [draft, setDraft] = useState<FleetSettings | null>(null);
  const key = (...p: string[]) => ["institution", "fleet", institutionId, ...p];
  const board = useQuery({ queryKey: key("board"), queryFn: () => api.board(institutionId), enabled: tab === "board" });
  const vehicles = useQuery({ queryKey: key("vehicles"), queryFn: () => api.vehicles(institutionId), enabled: tab === "vehicle" });
  const file = useQuery({ queryKey: key("file", vehicleId), queryFn: () => api.file(vehicleId), enabled: tab === "vehicle" && vehicleId !== "" });
  const fuel = useQuery({ queryKey: key("fuel", vehicleId), queryFn: () => api.fuel(vehicleId), enabled: tab === "vehicle" && vehicleId !== "" });
  const costs = useQuery({ queryKey: key("costs"), queryFn: () => api.costs(institutionId), enabled: tab === "costs" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId), enabled: tab === "settings" });
  const shown = draft ?? settings.data ?? null;
  const unit = file.data?.meterUnit ?? "km";

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-fleet-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );

  return (
    <BentoTile span={12} title="Fleet maintenance" data-slice="2D" data-fleet-maintenance action={tabs}>
      {tab === "board" && board.data && (
        <>
          <KeyValue label="Needs attention" value={`${board.data.overdue} overdue · ${board.data.dueSoon} due soon`} />
          <DataTable caption="Service and renewals due" rows={board.data.items} rowKey={(r) => `${r.kind}:${r.id}`}
            empty={<EmptyState icon={Wrench} title="All caught up">No service or renewal is due.</EmptyState>}
            columns={[
              { key: "v", header: "Vehicle", render: (r) => r.registrationNo },
              { key: "t", header: "What", render: (r) => <span data-due={r.title}>{r.title}</span> },
              { key: "d", header: "Due", render: (r) => dueLine(r) || "—" },
              { key: "s", header: "Status", render: (r) => badge(r.status) },
              { key: "a", header: "", render: (r) => (r.status !== "snoozed" ? (
                <Button size="sm" variant="ghost" data-action="snooze" disabled={busy} onClick={() => {
                  const until = new Date(Date.now() + 7 * 86_400_000).toISOString().slice(0, 10);
                  void run(() => (r.kind === "service" ? api.snoozeReminder(r.id, until) : api.snoozeRenewal(r.id, until)), "Snoozed for a week");
                }}>Snooze a week</Button>
              ) : null) },
            ]} />
        </>
      )}

      {tab === "vehicle" && (
        <>
          <FormSelect label="Vehicle" value={vehicleId} onChange={(e) => setVehicleId(e.target.value)}
            options={[{ value: "", label: "Choose a vehicle" }, ...(vehicles.data ?? []).map((v) => ({ value: v.id, label: `${v.registrationNo} · ${v.label}` }))]} />
          {file.data && (
            <>
              <KeyValue label="Meter" value={file.data.meter === null ? "Not recorded" : `${file.data.meter.toLocaleString("en-IN")} ${unit}`} />
              <FormGrid columns={2}>
                <TextField label={`New meter reading (${unit})`} inputMode="decimal" value={meter} onChange={(e) => setMeter(e.target.value)} />
                <Button size="sm" data-action="meter" disabled={busy || !meter.trim()} onClick={() => void run(async () => { await api.meter(vehicleId, meter.trim()); setMeter(""); }, "Meter recorded")}>Record reading</Button>
              </FormGrid>

              <DataTable caption="Service reminders" rows={file.data.reminders} rowKey={(r) => r.id}
                empty={<EmptyState icon={Wrench} title="No reminders" compact>Add oil changes, tyre rotation, brake checks…</EmptyState>}
                columns={[
                  { key: "t", header: "Task", render: (r) => r.task },
                  { key: "s", header: "Schedule", render: (r) => r.schedule },
                  { key: "d", header: "Next", render: (r) => dueLine(r.state, unit) || "—" },
                  { key: "c", header: "On time", render: (r) => (r.compliance === null ? "—" : `${Math.round(r.compliance * 100)}%`) },
                  { key: "st", header: "Status", render: (r) => badge(r.state.status) },
                ]} />
              <FormGrid columns={4}>
                <TextField label="Task" value={rem.task} placeholder="Engine oil" onChange={(e) => setRem({ ...rem, task: e.target.value })} />
                <TextField label="Every (time)" inputMode="numeric" value={rem.every} onChange={(e) => setRem({ ...rem, every: e.target.value })} />
                <FormSelect label="Unit" value={rem.unit} onChange={(e) => setRem({ ...rem, unit: e.target.value })} options={UNITS.map((u) => ({ value: u, label: u }))} />
                <TextField label={`…or every (${unit})`} inputMode="numeric" value={rem.km} onChange={(e) => setRem({ ...rem, km: e.target.value })} />
              </FormGrid>
              <Button size="sm" data-action="add-reminder" disabled={busy || !rem.task.trim() || (!wholeOrNull(rem.every) && !wholeOrNull(rem.km))} onClick={() => void run(async () => {
                const every = wholeOrNull(rem.every);
                await api.addReminder(vehicleId, { task: rem.task.trim(), interval: every ? { every, unit: rem.unit } : null, meterInterval: wholeOrNull(rem.km) });
                setRem({ task: "", every: "", unit: "months", km: "" });
              }, "Reminder added")}>Add reminder</Button>

              <DataTable caption="Renewals" rows={file.data.renewals} rowKey={(r) => r.id}
                empty={<EmptyState icon={Wrench} title="No renewals" compact>Track RC, insurance, fitness, PUC and permits.</EmptyState>}
                columns={[
                  { key: "t", header: "Document", render: (r) => r.label ?? RENEWAL_TYPES.find((x) => x.value === r.renewalType)?.label ?? r.renewalType },
                  { key: "d", header: "Due", render: (r) => (r.done ? "Done" : r.dueDate ?? "—") },
                  { key: "s", header: "Status", render: (r) => (r.done ? <Badge kind="done">Done</Badge> : badge(r.status)) },
                  { key: "a", header: "", render: (r) => (r.done ? null : (
                    <Button size="sm" variant="ghost" data-action="renew" disabled={busy} onClick={() => void run(() => api.completeRenewal(r.id, r.version, {}), "Renewal recorded")}>Renewed</Button>
                  )) },
                ]} />
              <FormGrid columns={4}>
                <FormSelect label="Document" value={ren.type} onChange={(e) => setRen({ ...ren, type: e.target.value })} options={RENEWAL_TYPES} />
                <TextField label="Due date" type="date" value={ren.due} onChange={(e) => setRen({ ...ren, due: e.target.value })} />
                <TextField label="Repeats every" inputMode="numeric" value={ren.every} onChange={(e) => setRen({ ...ren, every: e.target.value })} />
                <FormSelect label="Unit" value={ren.unit} onChange={(e) => setRen({ ...ren, unit: e.target.value })} options={UNITS.map((u) => ({ value: u, label: u }))} />
              </FormGrid>
              <Button size="sm" data-action="add-renewal" disabled={busy || !ren.due} onClick={() => void run(async () => {
                const every = wholeOrNull(ren.every);
                await api.addRenewal(vehicleId, { renewalType: ren.type, dueDate: ren.due, repeat: every ? { every, unit: ren.unit } : null });
                setRen({ ...ren, due: "" });
              }, "Renewal added")}>Add renewal</Button>

              <FormGrid columns={4}>
                <TextField label="Service done" value={svc.description} placeholder="Oil and filter" onChange={(e) => setSvc({ ...svc, description: e.target.value })} />
                <TextField label={`Meter (${unit})`} inputMode="decimal" value={svc.meter} onChange={(e) => setSvc({ ...svc, meter: e.target.value })} />
                <TextField label="Amount (₹)" inputMode="decimal" value={svc.amount} onChange={(e) => setSvc({ ...svc, amount: e.target.value })} />
                <TextField label="Garage" value={svc.vendor} onChange={(e) => setSvc({ ...svc, vendor: e.target.value })} />
              </FormGrid>
              {file.data.reminders.filter((r) => r.active).map((r) => (
                <Checkbox key={r.id} label={`Completes: ${r.task}`} checked={svc.reminderIds.includes(r.id)} onChange={() => setSvc({ ...svc, reminderIds: svc.reminderIds.includes(r.id) ? svc.reminderIds.filter((x) => x !== r.id) : [...svc.reminderIds, r.id] })} />
              ))}
              <Button size="sm" data-action="add-service" disabled={busy || !svc.description.trim() || !svc.amount.trim()} onClick={() => void run(async () => {
                await api.addService(vehicleId, { description: svc.description.trim(), meter: svc.meter.trim() || null, amount: svc.amount.trim(), vendor: svc.vendor.trim() || null, reminderIds: svc.reminderIds });
                setSvc({ description: "", meter: "", amount: "", vendor: "", reminderIds: [] });
              }, "Service recorded")}>Record service</Button>

              {fuel.data && (
                <>
                  <KeyValue label="Average economy" value={fuel.data.fuel.avgEconomy === null ? "Needs two full fills" : `${fuel.data.fuel.avgEconomy.toFixed(2)} ${fuel.data.economyUnit}`} />
                  <DataTable caption="Fuel log" rows={fuel.data.items.slice(0, 20)} rowKey={(r) => r.id}
                    empty={<EmptyState icon={Wrench} title="No fill-ups" compact>Fill-ups give economy between full tanks.</EmptyState>}
                    columns={[
                      { key: "d", header: "Date", render: (r) => r.filledOn },
                      { key: "m", header: "Meter", render: (r) => (r.meter === null ? "—" : r.meter.toLocaleString("en-IN")) },
                      { key: "v", header: "Volume", render: (r) => `${r.volume} ${fuel.data!.volumeUnit}${r.partial ? " (partial)" : ""}` },
                      { key: "t", header: "Total", render: (r) => formatInr(r.total) },
                      { key: "e", header: "Economy", render: (r) => (r.derived.economy === null ? "—" : `${r.derived.economy.toFixed(2)} ${fuel.data!.economyUnit}`) },
                    ]} />
                </>
              )}
              <FormGrid columns={4}>
                <TextField label={`Meter (${unit})`} inputMode="decimal" value={fill.meter} onChange={(e) => setFill({ ...fill, meter: e.target.value })} />
                <TextField label="Litres" inputMode="decimal" value={fill.volume} onChange={(e) => setFill({ ...fill, volume: e.target.value })} />
                <TextField label="₹ per litre" inputMode="decimal" value={fill.price} onChange={(e) => setFill({ ...fill, price: e.target.value })} />
                <Toggle label="Partial fill" checked={fill.partial} onChange={(e) => setFill({ ...fill, partial: e.target.checked })} />
              </FormGrid>
              {warnings.map((w) => <p key={w} role="alert">{w}</p>)}
              <Button size="sm" data-action="add-fuel" disabled={busy || !fill.volume.trim() || !fill.price.trim()} onClick={() => void run(async () => {
                const r = await api.addFuel(vehicleId, { meter: fill.meter.trim() || null, volume: fill.volume.trim(), price: fill.price.trim(), partial: fill.partial });
                setWarnings(r.warnings);
                setFill({ meter: "", volume: "", price: fill.price, partial: false });
              }, "Fill-up recorded")}>Record fill-up</Button>
            </>
          )}
        </>
      )}

      {tab === "costs" && costs.data && (
        <>
          <KeyValue label={`${costs.data.from} to ${costs.data.to}`} value={formatInr(costs.data.total)} />
          <DataTable caption="Operating cost by vehicle" rows={costs.data.items} rowKey={(r) => r.vehicleId}
            empty={<EmptyState icon={Wrench} title="No costs yet">Fuel, services and expenses add up here.</EmptyState>}
            columns={[
              { key: "v", header: "Vehicle", render: (r) => r.registrationNo },
              { key: "f", header: "Fuel", render: (r) => formatInr(r.fuel) },
              { key: "s", header: "Service", render: (r) => formatInr(r.service) },
              { key: "o", header: "Other", render: (r) => formatInr(r.other) },
              { key: "t", header: "Total", render: (r) => formatInr(r.total) },
              { key: "u", header: "Usage", render: (r) => (r.usage === null ? "—" : `${r.usage.toLocaleString("en-IN")} ${r.meterUnit}`) },
              { key: "c", header: "Per km", render: (r) => (r.costPerMeter === null ? "—" : formatInr(r.costPerMeter)) },
            ]} />
        </>
      )}

      {tab === "settings" && shown && (
        <>
          <FormGrid columns={3}>
            <TextField label="Due soon (days ahead)" inputMode="numeric" value={String(shown.dueSoonDays)} onChange={(e) => setDraft({ ...shown, dueSoonDays: Number(e.target.value) })} />
            <TextField label="Due soon (km to go)" inputMode="decimal" value={shown.dueSoonMeter} onChange={(e) => setDraft({ ...shown, dueSoonMeter: e.target.value })} />
            <FormSelect label="Fuel measured in" value={shown.volumeUnit} onChange={(e) => setDraft({ ...shown, volumeUnit: e.target.value })} options={[{ value: "L", label: "Litres" }, { value: "gal", label: "Gallons" }]} />
          </FormGrid>
          <Button size="sm" data-action="save-fleet-settings" disabled={busy || !draft} onClick={() => void run(async () => {
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
