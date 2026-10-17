/**
 * ERP_EXTENSIONS 2B: service-desk automation (ported from Everything's help-desk rules, MIT).
 * The SLA board lists open tickets by the target due soonest; queues are saved condition sets;
 * rules tag, prioritise, categorise and assign tickets as they are raised or changed from any
 * campus module; SLA targets set first-response and resolution times per priority.
 */
import { Filter } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import { createHttpDeskApi, type DeskApi } from "./api";
import { ACTIONS, conditionValue, describeAction, describeCondition, FIELDS, METRIC_LABEL, OP_LABEL, PRIORITIES, targetProblem, toHours, toMinutes, valueChoices, words } from "./desk";
import { createMockDeskApi } from "./mock";
import type { Action, Condition, QueueRow, SlaTarget } from "./types";

let fallback: DeskApi | null = null;
export function defaultDeskApi(): DeskApi {
  fallback ??= USES_API ? createHttpDeskApi() : createMockDeskApi();
  return fallback;
}

type Tab = "board" | "queues" | "rules" | "sla";
const TABS: { value: Tab; label: string }[] = [
  { value: "board", label: "SLA board" },
  { value: "queues", label: "Queues" },
  { value: "rules", label: "Rules" },
  { value: "sla", label: "SLA targets" },
];

interface CondDraft {
  field: string;
  op: string;
  value: string;
}
const blankCond = (): CondDraft => ({ field: "subject", op: "contains", value: "" });
const toCondition = (c: CondDraft): Condition => ({ field: c.field, op: c.op, value: conditionValue(c.field, c.value) });

function ConditionRow({ c, onChange, onRemove }: { c: CondDraft; onChange: (c: CondDraft) => void; onRemove?: () => void }) {
  const f = FIELDS.find((x) => x.value === c.field) ?? FIELDS[0]!;
  const choices = valueChoices(c.field);
  const noValue = c.op === "changed" || c.op === "present" || c.op === "absent";
  return (
    <FormGrid columns={4}>
      <FormSelect label="When" value={c.field} onChange={(e) => {
        const nf = FIELDS.find((x) => x.value === e.target.value)!;
        onChange({ field: nf.value, op: nf.ops[0]!, value: valueChoices(nf.value)?.[0] ?? "" });
      }} options={FIELDS.map((x) => ({ value: x.value, label: x.label }))} />
      <FormSelect label="Test" value={c.op} onChange={(e) => onChange({ ...c, op: e.target.value })} options={f.ops.map((o) => ({ value: o, label: OP_LABEL[o] ?? o }))} />
      {noValue ? <span /> : choices ? (
        <FormSelect label="Value" value={c.value} onChange={(e) => onChange({ ...c, value: e.target.value })} options={choices.map((v) => ({ value: v, label: words(v) }))} />
      ) : (
        <TextField label="Value" value={c.value} onChange={(e) => onChange({ ...c, value: e.target.value })} />
      )}
      {onRemove ? <Button size="sm" variant="ghost" onClick={onRemove}>Remove</Button> : <span />}
    </FormGrid>
  );
}

function slaBadge(r: QueueRow) {
  if (!r.sla) return <Badge kind="done">{words(r.ticket.status)}</Badge>;
  const label = `${METRIC_LABEL[r.sla.metric] ?? r.sla.metric} ${r.sla.breached ? `late by ${(r.slaIn ?? "").replace("-", "")}` : `in ${r.slaIn}`}`;
  return <Badge kind={r.sla.breached ? "attention" : "info"}>{label}</Badge>;
}

function TicketTable({ caption, rows }: { caption: string; rows: QueueRow[] }) {
  return (
    <DataTable caption={caption} rows={rows} rowKey={(r) => r.ticket.id}
      empty={<EmptyState icon={Filter} title="Nothing here">No tickets match right now.</EmptyState>}
      columns={[
        { key: "n", header: "Ticket", render: (r) => <span data-ticket={r.ticket.number}>{`${r.ticket.number} · ${r.ticket.title}`}</span> },
        { key: "p", header: "Priority", render: (r) => words(r.ticket.priority) },
        { key: "s", header: "Status", render: (r) => words(r.ticket.status) },
        { key: "a", header: "Assignee", render: (r) => r.ticket.assignee ?? "—" },
        { key: "t", header: "Tags", render: (r) => (r.tags.length ? r.tags.join(", ") : "—") },
        { key: "l", header: "SLA", render: (r) => slaBadge(r) },
      ]} />
  );
}

export function ServiceDeskRulesTile({ institutionId, api = defaultDeskApi() }: { institutionId: string; api?: DeskApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("board");
  const [viewId, setViewId] = useState("sys-unsolved");
  const [viewDraft, setViewDraft] = useState<{ title: string; cond: CondDraft }>({ title: "", cond: { field: "tags", op: "includes", value: "" } });
  const [rule, setRule] = useState<{ title: string; event: string; conds: CondDraft[]; actions: { field: string; value: string }[] }>({ title: "", event: "created", conds: [blankCond()], actions: [{ field: "priority", value: "high" }] });
  const [preview, setPreview] = useState<string | null>(null);
  const [targets, setTargets] = useState<Record<string, SlaTarget> | null>(null);
  const key = (...p: string[]) => ["institution", "service-desk", institutionId, ...p];
  const board = useQuery({ queryKey: key("board"), queryFn: () => api.board(institutionId), enabled: tab === "board" });
  const views = useQuery({ queryKey: key("views"), queryFn: () => api.views(institutionId), enabled: tab === "queues" });
  const queue = useQuery({ queryKey: key("view", viewId), queryFn: () => api.viewTickets(institutionId, viewId), enabled: tab === "queues" });
  const rules = useQuery({ queryKey: key("rules"), queryFn: () => api.rules(institutionId), enabled: tab === "rules" });
  const policy = useQuery({ queryKey: key("policy"), queryFn: () => api.policy(institutionId), enabled: tab === "sla" });
  const shown = targets ?? policy.data?.targets ?? {};

  const ruleInput = () => ({
    title: rule.title.trim(),
    event: rule.event,
    conditions: { all: rule.conds.filter((c) => c.value !== "" || c.op === "changed" || c.op === "present" || c.op === "absent").map(toCondition), any: [] },
    actions: rule.actions.filter((a) => a.value.trim() !== "").map((a): Action => ({ field: a.field, value: a.value.trim() })),
  });

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-desk-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );

  return (
    <BentoTile span={12} title="Service desk automation" data-slice="2B" data-service-desk-rules action={tabs}>
      {tab === "board" && board.data && (
        <>
          <KeyValue label="Open with a target" value={`${board.data.items.length} · ${board.data.breached} past due`} />
          <TicketTable caption="SLA board" rows={board.data.items} />
        </>
      )}

      {tab === "queues" && (
        <>
          <FormSelect label="Queue" value={viewId} onChange={(e) => setViewId(e.target.value)} options={(views.data ?? []).map((v) => ({ value: v.id, label: v.title }))} />
          {queue.data && <TicketTable caption={queue.data.title} rows={queue.data.items} />}
          {(views.data ?? []).filter((v) => !v.system && v.id === viewId).map((v) => (
            <Button key={v.id} size="sm" variant="ghost" data-action="delete-view" disabled={busy} onClick={() => void run(async () => { await api.deleteView(v.id); setViewId("sys-unsolved"); }, "Queue deleted")}>Delete this queue</Button>
          ))}
          <TextField label="New queue name" value={viewDraft.title} placeholder="Hostel complaints" onChange={(e) => setViewDraft({ ...viewDraft, title: e.target.value })} />
          <ConditionRow c={viewDraft.cond} onChange={(cond) => setViewDraft({ ...viewDraft, cond })} />
          <Button size="sm" data-action="save-view" disabled={busy || !viewDraft.title.trim()} onClick={() => void run(async () => {
            const v = await api.createView(institutionId, { title: viewDraft.title.trim(), conditions: { all: [toCondition(viewDraft.cond)], any: [] } });
            setViewId(v.id);
            setViewDraft({ ...viewDraft, title: "" });
            return v;
          }, "Queue saved")}>Save queue</Button>
        </>
      )}

      {tab === "rules" && (
        <>
          <DataTable caption="Rules, in run order" rows={rules.data ?? []} rowKey={(r) => r.id}
            empty={<EmptyState icon={Filter} title="No rules yet">Rules tag, prioritise and route tickets the moment they're raised.</EmptyState>}
            columns={[
              { key: "t", header: "Rule", render: (r) => <span data-rule={r.title}>{r.title}</span> },
              { key: "e", header: "Runs on", render: (r) => (r.event === "any" ? "Raised or changed" : r.event === "created" ? "Raised" : "Changed") },
              { key: "c", header: "When", render: (r) => [...r.conditions.all, ...r.conditions.any].map(describeCondition).join(" and ") || "Always" },
              { key: "a", header: "Then", render: (r) => r.actions.map(describeAction).join("; ") },
              { key: "s", header: "", render: (r) => (
                <span className="flex gap-1">
                  <Button size="sm" variant="ghost" data-action="toggle-rule" disabled={busy} onClick={() => void run(() => api.updateRule(r.id, r.version, { title: r.title, event: r.event, position: r.position, active: !r.active, conditions: r.conditions, actions: r.actions }), r.active ? "Rule paused" : "Rule on")}>{r.active ? "Pause" : "Turn on"}</Button>
                  <Button size="sm" variant="ghost" data-action="delete-rule" disabled={busy} onClick={() => void run(() => api.deleteRule(r.id), "Rule deleted")}>Delete</Button>
                </span>
              ) },
            ]} />
          <FormGrid columns={2}>
            <TextField label="Rule name" value={rule.title} placeholder="Leaks are urgent" onChange={(e) => setRule({ ...rule, title: e.target.value })} />
            <FormSelect label="Runs when a ticket is" value={rule.event} onChange={(e) => setRule({ ...rule, event: e.target.value })}
              options={[{ value: "created", label: "Raised" }, { value: "updated", label: "Changed" }, { value: "any", label: "Raised or changed" }]} />
          </FormGrid>
          {rule.conds.map((c, i) => (
            <ConditionRow key={i} c={c} onChange={(n) => setRule({ ...rule, conds: rule.conds.map((x, j) => (j === i ? n : x)) })}
              onRemove={rule.conds.length > 1 ? () => setRule({ ...rule, conds: rule.conds.filter((_, j) => j !== i) }) : undefined} />
          ))}
          <Button size="sm" variant="ghost" onClick={() => setRule({ ...rule, conds: [...rule.conds, blankCond()] })}>And also…</Button>
          {rule.actions.map((a, i) => {
            const choices = valueChoices(a.field);
            const set = (n: { field: string; value: string }) => setRule({ ...rule, actions: rule.actions.map((x, j) => (j === i ? n : x)) });
            return (
              <FormGrid key={i} columns={2}>
                <FormSelect label="Then" value={a.field} onChange={(e) => set({ field: e.target.value, value: valueChoices(e.target.value)?.[0] ?? "" })} options={ACTIONS} />
                {choices ? (
                  <FormSelect label="To" value={a.value} onChange={(e) => set({ ...a, value: e.target.value })} options={choices.map((v) => ({ value: v, label: words(v) }))} />
                ) : (
                  <TextField label={a.field === "assignee" ? "Technician or team" : "Tags"} value={a.value} placeholder={a.field === "assignee" ? "team.estate" : "plumbing urgent"} onChange={(e) => set({ ...a, value: e.target.value })} />
                )}
              </FormGrid>
            );
          })}
          <Button size="sm" variant="ghost" onClick={() => setRule({ ...rule, actions: [...rule.actions, { field: "add_tags", value: "" }] })}>Add an action</Button>
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" data-action="preview-rule" disabled={busy} onClick={() => void run(async () => {
              const p = await api.preview(institutionId, ruleInput());
              setPreview(p.matched ? `Matches ${p.matched} open ticket${p.matched === 1 ? "" : "s"} now: ${p.hits.slice(0, 5).map((h) => h.number).join(", ")}` : "Matches no open ticket right now");
              return p;
            })}>Preview</Button>
            <Button size="sm" data-action="save-rule" disabled={busy || !rule.title.trim()} onClick={() => void run(async () => {
              const r = await api.createRule(institutionId, ruleInput());
              setRule({ title: "", event: "created", conds: [blankCond()], actions: [{ field: "priority", value: "high" }] });
              setPreview(null);
              return r;
            }, "Rule saved")}>Save rule</Button>
          </div>
          {preview && <p className="text-sm" role="status" data-rule-preview>{preview}</p>}
        </>
      )}

      {tab === "sla" && policy.data && (
        <>
          <KeyValue label="Targets" value={policy.data.customised ? "Set for this institution" : "Defaults (not saved yet)"} />
          <DataTable caption="SLA targets (hours)" rows={[...PRIORITIES]} rowKey={(p) => p}
            columns={[
              { key: "p", header: "Priority", render: (p) => words(p) },
              ...(["first", "next", "resolve"] as const).map((m) => ({
                key: m,
                header: METRIC_LABEL[m]!,
                render: (p: string) => (
                  <TextField label={`${words(p)} ${METRIC_LABEL[m]}`} value={toHours(shown[p]?.[m] ?? 0)} data-sla={`${p}-${m}`}
                    onChange={(e) => setTargets({ ...shown, [p]: { ...(shown[p] ?? { first: 0, next: 0, resolve: 0 }), [m]: toMinutes(e.target.value) } })} />
                ),
              })),
              { key: "x", header: "", render: (p) => { const e = shown[p] ? targetProblem(shown[p]!) : null; return e ? <Badge kind="attention">{e}</Badge> : null; } },
            ]} />
          <Button size="sm" data-action="save-sla" disabled={busy || PRIORITIES.some((p) => shown[p] && targetProblem(shown[p]!))} onClick={() => void run(async () => {
            const r = await api.savePolicy(institutionId, { enabled: policy.data!.enabled, targets: shown, version: policy.data!.version });
            setTargets(null);
            return r;
          }, "SLA targets saved")}>Save targets</Button>
        </>
      )}
      {dialog}
    </BentoTile>
  );
}
