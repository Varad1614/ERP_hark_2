/**
 * ERP_EXTENSIONS 3C: contracts and MoUs (term clock, renewals and obligations ported from
 * Everything's contract manager, MIT). Overview shows what waits on a decision, what renews soon
 * and what is owed; Contracts lists the book and opens one to record a decision, tick off
 * obligations and log MoU activity; NAAC shows functional MoUs per AQAR year; Settings holds the
 * horizons and calendar reminders.
 */
import { FileSignature } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, Toggle } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../../lib/api";
import { useCommand } from "../../../institution/hooks";
import { formatInr } from "../../../finance";
import { createHttpContractsApi, type ContractsApi } from "./api";
import { ACTIVITY_KINDS, activityLabel, aqarYear, aqarYears, clockText, DECISIONS, dueText, kindLabel, KINDS, ownerLabel, phaseKind, totalsText } from "./contracts";
import { createMockContractsApi } from "./mock";
import type { ContractKind, ContractSettings, DecisionKind, Owner, RepeatEvery } from "./types";

let fallback: ContractsApi | null = null;
export function defaultContractsApi(): ContractsApi {
  fallback ??= USES_API ? createHttpContractsApi() : createMockContractsApi();
  return fallback;
}

type Tab = "overview" | "contracts" | "naac" | "settings";
const TABS: { value: Tab; label: string }[] = [
  { value: "overview", label: "Overview" },
  { value: "contracts", label: "Contracts" },
  { value: "naac", label: "NAAC MoUs" },
  { value: "settings", label: "Settings" },
];

const today = () => new Date().toISOString().slice(0, 10);
const inr = (v: number) => formatInr(v.toFixed(2));
const blankContract = { title: "", kind: "amc" as ContractKind, counterpartyName: "", value: "", billing: "annual", startOn: "", endOn: "", autoRenew: false, termMonths: "12", noticeDays: "", scope: "national" };
const blankOb = { text: "", owner: "us" as Owner, dueOn: "", every: "none" as RepeatEvery | "none", count: "" };
const blankAct = { heldOn: today(), kind: "internship", title: "", students: "", staff: "", evidenceRef: "" };

export function ContractsTile({ institutionId, api = defaultContractsApi() }: { institutionId: string; api?: ContractsApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("overview");
  const [openId, setOpenId] = useState("");
  const [onlyMou, setOnlyMou] = useState(false);
  const [search, setSearch] = useState("");
  const [draft, setDraft] = useState(blankContract);
  const [ob, setOb] = useState(blankOb);
  const [act, setAct] = useState(blankAct);
  const [decision, setDecision] = useState<{ kind: DecisionKind; note: string; until: string }>({ kind: "renewing", note: "", until: "" });
  const [year, setYear] = useState("");
  const [settingsDraft, setSettingsDraft] = useState<ContractSettings | null>(null);
  const key = (...p: string[]) => ["institution", "contracts", institutionId, ...p];
  const dash = useQuery({ queryKey: key("dashboard"), queryFn: () => api.dashboard(institutionId), enabled: tab === "overview" });
  const list = useQuery({ queryKey: key("list", String(onlyMou), search), queryFn: () => api.list(institutionId, { kind: onlyMou ? "mou" : undefined, q: search || undefined }), enabled: tab === "contracts" });
  const one = useQuery({ queryKey: key("one", openId), queryFn: () => api.get(openId), enabled: tab === "contracts" && openId !== "" });
  const evidence = useQuery({ queryKey: key("naac", year), queryFn: () => api.evidence(institutionId, year || undefined), enabled: tab === "naac" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId), enabled: tab === "settings" });
  const shownSettings = settingsDraft ?? settings.data ?? null;
  const d = one.data;

  const open = (id: string) => {
    setOpenId(id);
    setTab("contracts");
  };
  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-contracts-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );
  const clockBadge = (c: Parameters<typeof clockText>[0] & Parameters<typeof phaseKind>[0]) => <Badge kind={phaseKind(c)}>{clockText(c)}</Badge>;

  const addContract = () => {
    const n = (s: string) => (s.trim() === "" ? undefined : Number(s));
    return run(async () => {
      const r = await api.create(institutionId, {
        title: draft.title, kind: draft.kind, counterpartyName: draft.counterpartyName || null, counterpartyScope: draft.scope as "national" | "international",
        value: draft.value || null, billing: draft.billing as "annual", startOn: draft.startOn || null, endOn: draft.endOn || null,
        autoRenew: draft.autoRenew, termMonths: n(draft.termMonths), noticeDays: n(draft.noticeDays),
      });
      setDraft(blankContract);
      setOpenId(r.contract.id);
      return r;
    }, "Contract added");
  };

  return (
    <BentoTile span={12} title="Contracts and MoUs" data-slice="3C" data-contracts action={tabs}>
      {tab === "overview" && dash.data && (
        <>
          <FormGrid columns={3}>
            <KeyValue label="Active" value={`${dash.data.stats.active} (${dash.data.stats.expiring60} ending within 60 days)`} />
            <KeyValue label="Yearly value" value={totalsText(dash.data.stats.annualByCurrency, inr)} />
            <KeyValue label="Open obligations" value={String(dash.data.stats.openObligations)} />
          </FormGrid>
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" data-action="reconcile-terms" disabled={busy}
              onClick={() => void run(() => api.reconcile(institutionId), (r) => (r.contractsUpdated ? `${r.termsRecorded} term${r.termsRecorded === 1 ? "" : "s"} recorded, ${r.terminated} closed out` : "Already up to date"))}>
              Record finished terms
            </Button>
            <a href={api.calendarUrl(institutionId)} data-action="calendar" download>Download deadlines (.ics)</a>
          </div>
          <DataTable caption="Waiting on a decision" rows={dash.data.attention} rowKey={(c) => c.id}
            empty={<EmptyState icon={FileSignature} title="Nothing waiting" compact>No notice window is open without a decision.</EmptyState>}
            columns={[
              { key: "t", header: "Contract", render: (c) => c.title },
              { key: "p", header: "Counterparty", render: (c) => c.counterpartyName },
              { key: "c", header: "Clock", render: (c) => clockBadge(c) },
              { key: "o", header: "", render: (c) => <Button size="sm" variant="ghost" data-action="open-contract" onClick={() => open(c.id)}>Open</Button> },
            ]} />
          <DataTable caption="Ending or renewing soon" rows={dash.data.upcoming} rowKey={(c) => c.id}
            empty={<EmptyState icon={FileSignature} title="Nothing soon" compact>No term ends inside the horizon.</EmptyState>}
            columns={[
              { key: "t", header: "Contract", render: (c) => c.title },
              { key: "k", header: "Type", render: (c) => kindLabel(c.kind) },
              { key: "c", header: "Clock", render: (c) => clockBadge(c) },
            ]} />
          <DataTable caption="Obligations due" rows={dash.data.dueObligations} rowKey={(r) => r.obligation.id}
            empty={<EmptyState icon={FileSignature} title="Nothing due" compact>No open obligation is overdue or due soon.</EmptyState>}
            columns={[
              { key: "x", header: "What", render: (r) => r.obligation.text },
              { key: "c", header: "Contract", render: (r) => r.contractTitle },
              { key: "w", header: "Owed by", render: (r) => ownerLabel(r.obligation.owner) },
              { key: "d", header: "When", render: (r) => <Badge kind={(r.obligation.daysLeft ?? 0) < 0 ? "attention" : "info"}>{dueText(r.obligation)}</Badge> },
              { key: "a", header: "", render: (r) => <Button size="sm" variant="ghost" data-action="complete-obligation" disabled={busy} onClick={() => void run(() => api.complete(r.obligation.id), "Marked done")}>Done</Button> },
            ]} />
        </>
      )}

      {tab === "contracts" && (
        <>
          <FormGrid columns={2}>
            <TextField label="Search" value={search} placeholder="Title, counterparty, tag" onChange={(e) => setSearch(e.target.value)} />
            <Toggle label="MoUs only" checked={onlyMou} onChange={(e) => setOnlyMou(e.target.checked)} />
          </FormGrid>
          <DataTable caption="Contracts" rows={list.data ?? []} rowKey={(c) => c.id}
            empty={<EmptyState icon={FileSignature} title="No contracts" compact>Add the first one below.</EmptyState>}
            columns={[
              { key: "t", header: "Contract", render: (c) => c.title },
              { key: "k", header: "Type", render: (c) => kindLabel(c.kind) },
              { key: "p", header: "Counterparty", render: (c) => c.counterpartyName },
              { key: "v", header: "Value", render: (c) => (c.value === null ? "—" : c.currency === "INR" ? formatInr(c.value) : `${c.currency} ${c.value}`) },
              { key: "c", header: "Clock", render: (c) => clockBadge(c) },
              { key: "o", header: "", render: (c) => <Button size="sm" variant={c.id === openId ? "primary" : "ghost"} data-action="open-contract" onClick={() => setOpenId(c.id)}>Open</Button> },
            ]} />

          {d && (
            <div data-contract-detail={d.contract.id}>
              <KeyValue label={d.contract.title} value={`${kindLabel(d.contract.kind)} · ${d.contract.counterpartyName}${d.contract.counterpartyScope === "international" ? " (international)" : ""} · ${clockText(d.contract)}`} />
              {d.contract.status === "active" && d.contract.renewal.date && (
                <FormGrid columns={3}>
                  <FormSelect label="Decision for this term end" value={decision.kind} onChange={(e) => setDecision({ ...decision, kind: e.target.value as DecisionKind })} options={DECISIONS} />
                  <TextField label={decision.kind === "snoozed" ? "Snooze until" : "Note"} type={decision.kind === "snoozed" ? "date" : "text"}
                    value={decision.kind === "snoozed" ? decision.until : decision.note}
                    onChange={(e) => setDecision(decision.kind === "snoozed" ? { ...decision, until: e.target.value } : { ...decision, note: e.target.value })} />
                  <div className="flex gap-1">
                    <Button size="sm" data-action="record-decision" disabled={busy || (decision.kind === "snoozed" && !decision.until)}
                      onClick={() => void run(() => api.decide(d.contract.id, { kind: decision.kind, note: decision.note, until: decision.until || null }), "Decision recorded")}>Record</Button>
                    {d.contract.renewal.decision && (
                      <Button size="sm" variant="ghost" data-action="clear-decision" disabled={busy} onClick={() => void run(() => api.decide(d.contract.id, { kind: null }), "Decision cleared")}>Clear</Button>
                    )}
                  </div>
                </FormGrid>
              )}
              <a href={api.calendarUrl(institutionId, d.contract.id)} download>This contract's deadlines (.ics)</a>

              <DataTable caption="Obligations" rows={d.obligations} rowKey={(o) => o.id}
                empty={<EmptyState icon={FileSignature} title="No obligations" compact>Add who owes what, and when.</EmptyState>}
                columns={[
                  { key: "x", header: "What", render: (o) => o.text },
                  { key: "w", header: "Owed by", render: (o) => ownerLabel(o.owner) },
                  { key: "d", header: "When", render: (o) => <Badge kind={o.done ? "done" : (o.daysLeft ?? 0) < 0 ? "attention" : "info"}>{dueText(o)}</Badge> },
                  { key: "a", header: "", render: (o) => (
                    <div className="flex gap-1">
                      {!o.done && <Button size="sm" variant="ghost" data-action="complete-obligation" disabled={busy} onClick={() => void run(() => api.complete(o.id), "Marked done")}>Done</Button>}
                      {(o.done || o.completed > 0) && <Button size="sm" variant="ghost" data-action="undo-obligation" disabled={busy} onClick={() => void run(() => api.undo(o.id), "Undone")}>Undo</Button>}
                      <Button size="sm" variant="ghost" data-action="remove-obligation" disabled={busy} onClick={() => void run(() => api.removeObligation(o.id), "Removed")}>Remove</Button>
                    </div>
                  ) },
                ]} />
              <FormGrid columns={3}>
                <TextField label="Obligation" value={ob.text} placeholder="Quarterly service report" onChange={(e) => setOb({ ...ob, text: e.target.value })} />
                <FormSelect label="Owed by" value={ob.owner} onChange={(e) => setOb({ ...ob, owner: e.target.value as Owner })}
                  options={[{ value: "us", label: "Us" }, { value: "them", label: "Counterparty" }, { value: "mutual", label: "Both" }]} />
                <TextField label="Due" type="date" value={ob.dueOn} onChange={(e) => setOb({ ...ob, dueOn: e.target.value })} />
                <FormSelect label="Repeats" value={ob.every} onChange={(e) => setOb({ ...ob, every: e.target.value as RepeatEvery | "none" })}
                  options={[{ value: "none", label: "Does not repeat" }, { value: "monthly", label: "Monthly" }, { value: "quarterly", label: "Quarterly" }, { value: "annually", label: "Annually" }]} />
                <TextField label="Times (blank = until the contract ends)" inputMode="numeric" value={ob.count} onChange={(e) => setOb({ ...ob, count: e.target.value })} />
              </FormGrid>
              <Button size="sm" variant="ghost" data-action="add-obligation" disabled={busy || !ob.text.trim() || (ob.every !== "none" && !ob.dueOn)}
                onClick={() => void run(async () => {
                  const r = await api.addObligation(d.contract.id, {
                    text: ob.text, owner: ob.owner, dueOn: ob.dueOn || null,
                    repeat: ob.every === "none" ? null : { every: ob.every, count: ob.count ? Number(ob.count) : null, contractEnd: !ob.count },
                  });
                  setOb(blankOb);
                  return r;
                }, "Obligation added")}>Add obligation</Button>

              {d.terms.length > 0 && (
                <DataTable caption="Term history" rows={d.terms} rowKey={(t) => t.endOn}
                  columns={[
                    { key: "s", header: "From", render: (t) => t.startOn ?? "—" },
                    { key: "e", header: "To", render: (t) => t.endOn },
                    { key: "h", header: "Ended", render: (t) => t.ended.replace("-", " ") },
                    { key: "n", header: "Note", render: (t) => t.note || "—" },
                  ]} />
              )}

              <DataTable caption="Activities" rows={d.activities} rowKey={(a) => a.id}
                empty={<EmptyState icon={FileSignature} title="No activities" compact>Log internships, exchanges, joint research… they become NAAC evidence.</EmptyState>}
                columns={[
                  { key: "d", header: "Held on", render: (a) => a.heldOn },
                  { key: "k", header: "Kind", render: (a) => activityLabel(a.kind) },
                  { key: "t", header: "Activity", render: (a) => a.title },
                  { key: "p", header: "Students / staff", render: (a) => `${a.students} / ${a.staff}` },
                  { key: "x", header: "", render: (a) => <Button size="sm" variant="ghost" data-action="remove-activity" disabled={busy} onClick={() => void run(() => api.removeActivity(a.id), "Removed")}>Remove</Button> },
                ]} />
              <FormGrid columns={3}>
                <TextField label="Held on" type="date" value={act.heldOn} onChange={(e) => setAct({ ...act, heldOn: e.target.value })} />
                <FormSelect label="Kind" value={act.kind} onChange={(e) => setAct({ ...act, kind: e.target.value })} options={ACTIVITY_KINDS} />
                <TextField label="Activity" value={act.title} placeholder="Summer internship, batch 1" onChange={(e) => setAct({ ...act, title: e.target.value })} />
                <TextField label="Students" inputMode="numeric" value={act.students} onChange={(e) => setAct({ ...act, students: e.target.value })} />
                <TextField label="Staff" inputMode="numeric" value={act.staff} onChange={(e) => setAct({ ...act, staff: e.target.value })} />
                <TextField label="Evidence reference" value={act.evidenceRef} placeholder="IQAC file no." onChange={(e) => setAct({ ...act, evidenceRef: e.target.value })} />
              </FormGrid>
              <Button size="sm" variant="ghost" data-action="add-activity" disabled={busy || !act.title.trim() || act.heldOn > today()}
                onClick={() => void run(async () => {
                  const r = await api.addActivity(d.contract.id, { heldOn: act.heldOn, kind: act.kind, title: act.title, students: Number(act.students) || 0, staff: Number(act.staff) || 0, evidenceRef: act.evidenceRef || null });
                  setAct(blankAct);
                  return r;
                }, "Activity logged")}>Log activity</Button>
            </div>
          )}

          <FormGrid columns={3}>
            <TextField label="Title" value={draft.title} placeholder="RO plant AMC" onChange={(e) => setDraft({ ...draft, title: e.target.value })} />
            <FormSelect label="Type" value={draft.kind} onChange={(e) => setDraft({ ...draft, kind: e.target.value as ContractKind })} options={KINDS} />
            <TextField label="Counterparty" value={draft.counterpartyName} placeholder="Vendor or partner institution" onChange={(e) => setDraft({ ...draft, counterpartyName: e.target.value })} />
            <TextField label="Value (₹)" inputMode="decimal" value={draft.value} onChange={(e) => setDraft({ ...draft, value: e.target.value })} />
            <FormSelect label="Billed" value={draft.billing} onChange={(e) => setDraft({ ...draft, billing: e.target.value })}
              options={[{ value: "one-time", label: "One-time" }, { value: "monthly", label: "Monthly" }, { value: "quarterly", label: "Quarterly" }, { value: "annual", label: "Annual" }]} />
            <FormSelect label="Partner" value={draft.scope} onChange={(e) => setDraft({ ...draft, scope: e.target.value })}
              options={[{ value: "national", label: "In India" }, { value: "international", label: "Abroad" }]} />
            <TextField label="Starts" type="date" value={draft.startOn} onChange={(e) => setDraft({ ...draft, startOn: e.target.value })} />
            <TextField label="First term ends" type="date" value={draft.endOn} onChange={(e) => setDraft({ ...draft, endOn: e.target.value })} />
            <TextField label="Notice (days)" inputMode="numeric" value={draft.noticeDays} placeholder="Institution default" onChange={(e) => setDraft({ ...draft, noticeDays: e.target.value })} />
            <Toggle label="Renews automatically" checked={draft.autoRenew} onChange={(e) => setDraft({ ...draft, autoRenew: e.target.checked })} />
            <TextField label="Renewal term (months)" inputMode="numeric" value={draft.termMonths} onChange={(e) => setDraft({ ...draft, termMonths: e.target.value })} />
          </FormGrid>
          <Button size="sm" data-action="add-contract" disabled={busy || !draft.title.trim() || !draft.counterpartyName.trim() || (draft.autoRenew && !draft.endOn)} onClick={() => void addContract()}>
            Add contract
          </Button>
        </>
      )}

      {tab === "naac" && (
        <>
          <FormSelect label="AQAR year (July–June)" value={year || evidence.data?.year || aqarYear(today())} onChange={(e) => setYear(e.target.value)}
            options={aqarYears(today()).map((y) => ({ value: y, label: y }))} />
          {evidence.data && (
            <>
              <FormGrid columns={3}>
                <KeyValue label="Functional MoUs" value={`${evidence.data.totals.functional} of ${evidence.data.totals.inForce} in force`} />
                <KeyValue label="Activities" value={String(evidence.data.totals.activities)} />
                <KeyValue label="Participants" value={`${evidence.data.totals.students} students, ${evidence.data.totals.staff} staff`} />
              </FormGrid>
              <DataTable caption={`MoUs in force, ${evidence.data.year}`} rows={evidence.data.mous} rowKey={(m) => m.contractId}
                empty={<EmptyState icon={FileSignature} title="No MoUs in force" compact>Add an MoU under Contracts.</EmptyState>}
                columns={[
                  { key: "t", header: "MoU", render: (m) => m.title },
                  { key: "p", header: "Partner", render: (m) => `${m.partner}${m.scope === "international" ? " (abroad)" : ""}` },
                  { key: "k", header: "Type", render: (m) => kindLabel(m.kind) },
                  { key: "a", header: "Activities", render: (m) => (m.activities.length ? m.activities.map((a) => activityLabel(a.kind)).join(", ") : "—") },
                  { key: "f", header: "", render: (m) => <Badge kind={m.functional ? "done" : "attention"}>{m.functional ? "Functional" : "No activity this year"}</Badge> },
                  { key: "o", header: "", render: (m) => <Button size="sm" variant="ghost" data-action="open-contract" onClick={() => open(m.contractId)}>Open</Button> },
                ]} />
            </>
          )}
        </>
      )}

      {tab === "settings" && shownSettings && (
        <>
          <FormGrid columns={3}>
            <TextField label="Ending soon horizon (days)" inputMode="numeric" value={String(shownSettings.soonDays)} onChange={(e) => setSettingsDraft({ ...shownSettings, soonDays: Number(e.target.value) || 0 })} />
            <TextField label="Obligations due within (days)" inputMode="numeric" value={String(shownSettings.obligationDays)} onChange={(e) => setSettingsDraft({ ...shownSettings, obligationDays: Number(e.target.value) || 0 })} />
            <TextField label="Default notice (days)" inputMode="numeric" value={String(shownSettings.defaultNoticeDays)} onChange={(e) => setSettingsDraft({ ...shownSettings, defaultNoticeDays: Number(e.target.value) || 0 })} />
            <TextField label="Calendar reminders (days before)" value={shownSettings.alarmDays.join(", ")}
              onChange={(e) => setSettingsDraft({ ...shownSettings, alarmDays: e.target.value.split(",").map((s) => Number(s.trim())).filter((n) => Number.isFinite(n) && n >= 0) })} />
          </FormGrid>
          <Button size="sm" data-action="save-contract-settings" disabled={busy || settingsDraft === null}
            onClick={() => void run(async () => {
              const r = await api.saveSettings(institutionId, shownSettings);
              setSettingsDraft(null);
              return r;
            }, "Settings saved")}>Save settings</Button>
        </>
      )}
      {dialog}
    </BentoTile>
  );
}
