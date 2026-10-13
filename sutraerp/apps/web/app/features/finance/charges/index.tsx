/**
 * Slice **5A** web components (fee heads and structures, charges, instalments, disputes,
 * adjustments, financial aid, finance configuration). The export names are imported by the route
 * frames (docs/batch5-contracts.md §10); real screens are marked with `data-slice="5A"`.
 *
 * - `ChargesAdminTab` — Admin → Finance → Fees & charges (fee heads, structures, charges,
 *   instalments, disputes, configuration and rules).
 * - `AdjustmentsAdminTab` — Admin → Finance → Adjustments (credit / debit notes, write-offs, waivers).
 * - `AidAdminTab` — Admin → Finance → Financial aid (handed-off scholarship decisions → allocations).
 * - `StudentChargesPanel` — student `/fees`: charges, instalments, concessions / aid.
 *
 * Money is always a decimal string from the API, shown with `formatInr` (never parsed to a float).
 */
import { BadgeIndianRupee, FileText, GraduationCap, Plus, Receipt, Scale, Settings2 } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, NoInstitution, REASON, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { formatInr } from "../index";
import { FINAL_CHARGE, fkeys, useChargesApi } from "./hooks";
import type { AdjustmentKind, ChargeCommand, ChargeDetail, ChargeListItem, FeeCategory, PolicyKind } from "./types";

export { ChargesApiProvider } from "./hooks";

const CATEGORIES: FeeCategory[] = ["tuition", "admission", "examination", "hostel", "transport", "library", "laboratory", "fine", "deposit", "miscellaneous"];
const KINDS: AdjustmentKind[] = ["credit_note", "debit_note", "concession", "waiver", "write_off"];
const opts = (xs: readonly string[]) => xs.map((x) => ({ value: x, label: statusLabel(x) }));
const words = (s: string) => statusLabel(s);

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="5A">
      <NoInstitution />
    </BentoTile>
  );
}

/** Lifecycle buttons a charge offers in its current status. */
function chargeActions(c: ChargeListItem): ChargeCommand[] {
  switch (c.status) {
    case "draft":
    case "pending_validation":
      return ["validate", "publish", "cancel"];
    case "validated":
      return ["publish", "cancel"];
    case "published":
    case "active":
      return c.outstandingAmount === "0.00" ? ["close"] : ["suspend", "cancel"];
    case "suspended":
      return ["resume", "cancel"];
    case "closed":
    case "cancelled":
    case "waived":
    case "written_off":
      return ["archive"];
    default:
      return [];
  }
}

// ================================================================ Fees & charges

export function ChargesAdminTab({ institution }: { institution: Institution | null }) {
  const api = useChargesApi();
  const { run, submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [selected, setSelected] = useState<string | null>(null);
  const [status, setStatus] = useState("");
  const iid = institution?.id;
  const heads = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "fee-heads"), queryFn: () => api.feeHeads(iid!), enabled: !!iid });
  const structures = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "fee-structures"), queryFn: () => api.structures(iid!), enabled: !!iid });
  const charges = useQuery({ queryKey: fkeys.scoped(`${iid}:${status}`, "charges"), queryFn: () => api.charges(iid!, { status: status || undefined }), enabled: !!iid });
  const detail = useQuery({ queryKey: fkeys.scoped(selected ?? "-", "charge"), queryFn: () => api.charge(selected!), enabled: !!selected });
  const disputes = useQuery({ queryKey: fkeys.scoped(selected ?? "-", "disputes"), queryFn: () => api.disputes(selected!), enabled: !!selected });
  if (!institution) return <NoInstitutionTile />;
  const headOptions = (heads.data ?? []).filter((h) => h.status === "active").map((h) => ({ value: h.id, label: `${h.code} · ${h.name}` }));
  const lineFields: FieldSpec[] = [
    { name: "feeHeadId", label: "Fee head", type: "select", options: headOptions, required: true, initial: headOptions[0]?.value },
    { name: "amount", label: "Amount (₹)", required: true, placeholder: "125000.00" },
  ];
  const c = detail.data;
  const command = (ch: ChargeListItem, cmd: ChargeCommand) => {
    if (cmd === "suspend" || cmd === "cancel")
      return ask({ title: `${words(cmd)} ${ch.chargeNumber}`, name: `charge-${cmd}`, submitLabel: words(cmd), fields: [REASON], onSubmit: (v) => submit(() => api.chargeCommand(ch.id, ch.version, cmd, v.reason), `${ch.chargeNumber}: ${words(cmd)}`) });
    return run(() => api.chargeCommand(ch.id, ch.version, cmd), `${ch.chargeNumber}: ${words(cmd)}`);
  };
  return (
    <>
      <BentoTile span={5} title="Fee heads" data-slice="5A" data-fee-heads action={
        <Button size="sm" variant="ghost" data-action="new-fee-head" onClick={() => ask({ title: "New fee head", name: "fee-head", submitLabel: "Create", fields: [
          { name: "code", label: "Code", required: true, placeholder: "HOSTEL" },
          { name: "name", label: "Name", required: true },
          { name: "category", label: "Category", type: "select", options: opts(CATEGORIES), initial: "miscellaneous", required: true },
        ], onSubmit: (v) => submit(() => api.createFeeHead(institution.id, { code: v.code!, name: v.name!, category: v.category as FeeCategory }), "Fee head created") })}>
          <Plus size={14} /> Fee head
        </Button>
      }>
        <DataTable caption="Fee heads" rows={heads.data ?? []} rowKey={(h) => h.id}
          empty={<EmptyState icon={Receipt} title="No fee heads">Create the heads charges are raised under.</EmptyState>}
          columns={[
            { key: "c", header: "Code", render: (h) => <span data-fee-head={h.code}>{h.code}</span> },
            { key: "n", header: "Name", render: (h) => h.name },
            { key: "a", header: "Income account", render: (h) => h.incomeAccountCode },
            { key: "s", header: "Status", render: (h) => <StatusBadge status={h.status} /> },
          ]} />
      </BentoTile>
      <BentoTile span={7} title="Fee structures" data-fee-structures action={
        <Button size="sm" variant="ghost" data-action="new-fee-structure" onClick={() => ask({ title: "New fee structure version", name: "fee-structure", submitLabel: "Create draft", fields: [
          { name: "code", label: "Code", required: true, placeholder: "BTECH-Y1" },
          { name: "name", label: "Name", required: true },
          { name: "dueDate", label: "Due date", type: "date" },
          ...lineFields,
        ], onSubmit: (v) => submit(() => api.createStructure(institution.id, { code: v.code!, name: v.name!, dueDate: v.dueDate || undefined, lines: [{ feeHeadId: v.feeHeadId!, amount: v.amount!, description: "Tuition" }] }), "Fee structure drafted") })}>
          <Plus size={14} /> Structure
        </Button>
      }>
        <DataTable caption="Fee structures" rows={structures.data ?? []} rowKey={(s) => s.id}
          empty={<EmptyState icon={FileText} title="No fee structures">Draft a structure, send it for approval, publish it and generate charges.</EmptyState>}
          columns={[
            { key: "c", header: "Structure", render: (s) => <span data-fee-structure={`${s.code}-v${s.versionNo}`}>{`${s.code} v${s.versionNo}`}</span> },
            { key: "t", header: "Total", render: (s) => formatInr(s.totalAmount) },
            { key: "g", header: "Charges", render: (s) => s.chargesGenerated },
            { key: "s", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "a",
              header: "",
              render: (s) => {
                const a: RowAction[] = [];
                if (s.status === "draft") a.push({ id: "submit-structure", label: "Submit", onClick: () => run(() => api.structureCommand(s.id, s.version, "submit"), "Sent for approval") });
                if (s.status === "approved") a.push({ id: "publish-structure", label: "Publish", variant: "primary", onClick: () => run(() => api.structureCommand(s.id, s.version, "publish"), "Fee structure published") });
                if (s.status === "published")
                  a.push({ id: "generate-charges", label: "Generate", variant: "primary", onClick: () => ask({ title: `Generate charges · ${s.code}`, name: "generate", submitLabel: "Generate and publish", fields: [{ name: "studentIds", label: "Student ids (comma separated)", required: true, wide: true }], onSubmit: (v) => submit(() => api.generate(s.id, v.studentIds!.split(",").map((x) => x.trim()).filter(Boolean)), (r) => `${r.created.length} charge(s) published, ${r.skipped.length} skipped`) }) });
                return <Actions actions={a} />;
              },
            },
          ]} />
      </BentoTile>
      <BentoTile span={selected ? 7 : 12} title="Student charges" data-charges action={
        <div className="sb-row-actions">
          <select aria-label="Status filter" value={status} onChange={(e) => setStatus(e.target.value)} data-charge-filter>
            <option value="">All statuses</option>
            {["draft", "validated", "published", "active", "suspended", "disputed", "closed", "cancelled", "written_off", "archived"].map((s) => <option key={s} value={s}>{words(s)}</option>)}
          </select>
          <Button size="sm" variant="ghost" data-action="new-charge" onClick={() => ask({ title: "Raise a charge", name: "charge", submitLabel: "Raise and publish", fields: [
            { name: "studentId", label: "Student id", required: true, wide: true },
            { name: "description", label: "Description", required: true, wide: true },
            { name: "dueDate", label: "Due date", type: "date" },
            ...lineFields,
          ], onSubmit: (v) => submit(() => api.createCharge(institution.id, { studentId: v.studentId!, description: v.description!, dueDate: v.dueDate || undefined, publish: true, lines: [{ feeHeadId: v.feeHeadId!, amount: v.amount!, description: v.description }] }), (r) => `${r.chargeNumber} published`) })}>
            <Plus size={14} /> Charge
          </Button>
        </div>
      }>
        <DataTable caption="Student charges" rows={charges.data?.items ?? []} rowKey={(x) => x.id}
          empty={<EmptyState icon={BadgeIndianRupee} title="No charges">Generate charges from a published fee structure or raise one.</EmptyState>}
          columns={[
            { key: "n", header: "Charge", render: (x) => <button type="button" className="sb-link" data-charge={x.chargeNumber} onClick={() => setSelected(x.id)}>{x.chargeNumber}</button> },
            { key: "p", header: "Student", render: (x) => `${x.displayName}${x.studentNumber ? ` · ${x.studentNumber}` : ""}` },
            { key: "t", header: "Total", render: (x) => formatInr(x.totalAmount) },
            { key: "o", header: "Outstanding", render: (x) => <span data-outstanding={x.chargeNumber}>{formatInr(x.outstandingAmount)}</span> },
            { key: "d", header: "Due", render: (x) => fmtDate(x.dueDate) },
            { key: "s", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
            { key: "a", header: "", render: (x) => <Actions actions={chargeActions(x).map((cmd) => ({ id: `charge-${cmd}`, label: words(cmd), variant: cmd === "publish" ? "primary" : "ghost", onClick: () => command(x, cmd) }))} /> },
          ]} />
      </BentoTile>
      {selected && c && (
        <BentoTile span={5} title={`${c.chargeNumber} · ${statusLabel(c.status)}`} data-charge-detail={c.chargeNumber} action={<Button size="sm" variant="ghost" onClick={() => setSelected(null)}>Close</Button>}>
          <p>{c.description}</p>
          <dl className="sb-facts">
            <dt>Total</dt><dd>{formatInr(c.totalAmount)}</dd>
            <dt>Adjustments</dt><dd>{formatInr(c.adjustmentAmount)}</dd>
            <dt>Settled</dt><dd>{formatInr(c.settledAmount)}</dd>
            <dt>Outstanding</dt><dd data-detail-outstanding>{formatInr(c.outstandingAmount)}</dd>
            <dt>Ledger entry</dt><dd>{c.journalEntryId ? "Posted" : "—"}</dd>
          </dl>
          <h3>Lines</h3>
          <ul>{c.lines.map((l) => <li key={l.lineNo}>{`${l.feeHeadCode} · ${l.description}: ${formatInr(l.amount)}`}</li>)}</ul>
          <h3>Instalments</h3>
          {c.instalments.length === 0 ? <p>No due schedule.</p> : (
            <ul data-instalments>{c.instalments.map((i) => <li key={i.id}>{`#${i.sequence} ${fmtDate(i.dueDate)} — ${formatInr(i.amount)} `}<StatusBadge status={i.status} /></li>)}</ul>
          )}
          <h3>Disputes</h3>
          {(disputes.data ?? []).length === 0 ? <p>None.</p> : (
            <ul>{(disputes.data ?? []).map((d) => (
              <li key={d.id} data-dispute={d.status}>
                {`${d.reason} `}<StatusBadge status={d.status} />
                {(d.status === "raised" || d.status === "under_review") && (
                  <Actions actions={[
                    { id: "uphold-dispute", label: "Uphold + waive", onClick: () => run(() => api.decideDispute(d.id, d.version, { decision: "approve", resolution: "waive", reason: "Upheld" }), "Dispute upheld") },
                    { id: "reject-dispute", label: "Reject", onClick: () => ask({ title: "Reject dispute", name: "reject-dispute", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideDispute(d.id, d.version, { decision: "reject", reason: v.reason }), "Dispute rejected") }) },
                  ]} />
                )}
              </li>
            ))}</ul>
          )}
          {!FINAL_CHARGE.includes(c.status) && c.status !== "closed" && (
            <Actions actions={[
              { id: "set-instalments", label: "Instalments", onClick: () => ask({ title: `Due schedule · ${c.chargeNumber}`, name: "instalments", submitLabel: "Set schedule", fields: [
                { name: "count", label: "Instalments", type: "number", required: true, initial: "2" },
                { name: "firstDueDate", label: "First due date", type: "date", required: true, initial: c.dueDate ?? "" },
              ], onSubmit: (v) => submit(() => api.setInstalments(c.id, { count: Number(v.count), firstDueDate: v.firstDueDate! }), "Due schedule set") }) },
              ...(["published", "active", "disputed"].includes(c.status) ? [{ id: "revise-charge", label: "Revise", onClick: () => ask({ title: `Revise ${c.chargeNumber}`, name: "revise", submitLabel: "Revise", fields: [
                { name: "newAmount", label: "New net amount (₹)", placeholder: "120000.00" },
                { name: "dueDate", label: "New due date", type: "date" },
                REASON,
              ], onSubmit: (v) => submit(() => api.revise(c.id, c.version, { reason: v.reason!, newAmount: v.newAmount || undefined, dueDate: v.dueDate || undefined }), "Charge revised") }) }] : []),
            ]} />
          )}
        </BentoTile>
      )}
      <PolicyTile institution={institution} kind="config" />
      <PolicyTile institution={institution} kind="rule" />
      {prompt}
      {dialog}
    </>
  );
}

/** Governed configuration / rule versions (draft → approval → effective, rollback = new version). */
function PolicyTile({ institution, kind }: { institution: Institution; kind: PolicyKind }) {
  const api = useChargesApi();
  const { run, submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const list = useQuery({ queryKey: fkeys.scoped(institution.id, `policy-${kind}`), queryFn: () => api.policies(institution.id, kind) });
  const title = kind === "config" ? "Finance configuration" : "Financial rules";
  return (
    <BentoTile span={6} title={title} data-policy={kind} action={
      <Button size="sm" variant="ghost" data-action={`new-${kind}`} onClick={() => ask(kind === "config"
        ? { title: "New configuration version", name: "config", submitLabel: "Create draft", fields: [
            { name: "defaultDueDays", label: "Default due days", type: "number", required: true, initial: "30" },
            { name: "maxInstalments", label: "Maximum instalments", type: "number", required: true, initial: "6" },
            { name: "allowPartialPayments", label: "Allow partial payments", type: "checkbox", initial: "true" },
          ], onSubmit: (v) => submit(() => api.createPolicy(institution.id, "config", { values: { defaultDueDays: Number(v.defaultDueDays), maxInstalments: Number(v.maxInstalments), allowPartialPayments: v.allowPartialPayments === "true" } }), "Configuration drafted") }
        : { title: "New late-fee rule version", name: "rule", submitLabel: "Create draft", fields: [
            { name: "percent", label: "Late fee %", required: true, initial: "2.00" },
            { name: "graceDays", label: "Grace days", type: "number", required: true, initial: "7" },
          ], onSubmit: (v) => submit(() => api.createPolicy(institution.id, "rule", { ruleKey: "late_fee", values: { percent: v.percent, graceDays: Number(v.graceDays) } }), "Rule drafted") })}>
        <Settings2 size={14} /> Version
      </Button>
    }>
      <DataTable caption={title} rows={list.data ?? []} rowKey={(p) => p.id}
        empty={<EmptyState icon={Settings2} title="No versions yet">Draft a version, approve and activate it.</EmptyState>}
        columns={[
          { key: "v", header: "Version", render: (p) => <span data-policy-version={`${kind}-${p.versionNo}`}>{`${p.ruleKey ? `${p.ruleKey} ` : ""}v${p.versionNo}${p.basedOnId ? " (rollback)" : ""}`}</span> },
          { key: "x", header: "Values", render: (p) => Object.entries(p.values).map(([k, val]) => `${k}: ${String(val)}`).join(", ") },
          { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
          {
            key: "a",
            header: "",
            render: (p) => {
              const a: RowAction[] = [];
              if (p.status === "draft") a.push({ id: `submit-${kind}`, label: "Submit", onClick: () => run(() => api.policyCommand(kind, p.id, p.version, "submit"), "Sent for approval") });
              if (p.status === "in_review" && p.approvedAt) a.push({ id: `activate-${kind}`, label: "Activate", variant: "primary", onClick: () => run(() => api.policyCommand(kind, p.id, p.version, "activate"), "Version effective") });
              if ((p.status === "superseded" || p.status === "rolled_back") && p.approvedAt)
                a.push({ id: `rollback-${kind}`, label: "Roll back to", onClick: () => ask({ title: `Roll back to v${p.versionNo}`, name: "rollback", submitLabel: "Create rollback draft", fields: [REASON], onSubmit: (v) => submit(() => api.rollback(kind, p.id, v.reason!), "Rollback version drafted") }) });
              return <Actions actions={a} />;
            },
          },
        ]} />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

// ================================================================ Adjustments

export function AdjustmentsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useChargesApi();
  const { run, submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const iid = institution?.id;
  const list = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "adjustments"), queryFn: () => api.adjustments(iid!), enabled: !!iid });
  const charges = useQuery({ queryKey: fkeys.scoped(`${iid}:`, "charges"), queryFn: () => api.charges(iid!), enabled: !!iid });
  if (!institution) return <NoInstitutionTile />;
  const chargeOptions = (charges.data?.items ?? []).filter((c) => ["published", "active", "disputed"].includes(c.status)).map((c) => ({ value: c.id, label: `${c.chargeNumber} · ${c.displayName} · ${formatInr(c.outstandingAmount)}` }));
  return (
    <>
      <BentoTile span={12} title="Adjustments" data-slice="5A" data-adjustments action={
        <Button size="sm" variant="ghost" data-action="new-adjustment" onClick={() => ask({ title: "New adjustment", name: "adjustment", submitLabel: "Create draft", fields: [
          { name: "chargeId", label: "Charge", type: "select", options: chargeOptions, required: true, initial: chargeOptions[0]?.value, wide: true },
          { name: "kind", label: "Kind", type: "select", options: opts(KINDS), initial: "credit_note", required: true },
          { name: "amount", label: "Amount (₹)", required: true, placeholder: "5000.00" },
          REASON,
        ], onSubmit: (v) => submit(() => api.createAdjustment(v.chargeId!, { kind: v.kind as AdjustmentKind, amount: v.amount!, reason: v.reason! }), (a) => `${a.adjustmentNumber} drafted`) })}>
          <Plus size={14} /> Adjustment
        </Button>
      }>
        <DataTable caption="Adjustments" rows={list.data ?? []} rowKey={(a) => a.id}
          empty={<EmptyState icon={Scale} title="No adjustments">Credit notes, debit notes, waivers and write-offs appear here once drafted.</EmptyState>}
          columns={[
            { key: "n", header: "Number", render: (a) => <span data-adjustment={a.adjustmentNumber}>{a.adjustmentNumber}</span> },
            { key: "c", header: "Charge", render: (a) => a.chargeNumber },
            { key: "k", header: "Kind", render: (a) => words(a.kind) },
            { key: "e", header: "Effect", render: (a) => formatInr(a.effect) },
            { key: "r", header: "Reason", render: (a) => a.reason },
            { key: "s", header: "Status", render: (a) => <span data-adjustment-status={a.adjustmentNumber}><StatusBadge status={a.status} /></span> },
            {
              key: "a",
              header: "",
              render: (a) => {
                const acts: RowAction[] = [];
                if (a.status === "draft") acts.push({ id: "submit-adjustment", label: "Submit", variant: "primary", onClick: () => run(() => api.submitAdjustment(a.id, a.version), (r) => `${r.adjustmentNumber}: ${statusLabel(r.status)}`) });
                if (a.status === "pending_approval") {
                  acts.push({ id: "approve-adjustment", label: "Approve", variant: "primary", onClick: () => run(() => api.decideAdjustment(a.id, a.version, "approve"), "Adjustment posted") });
                  acts.push({ id: "reject-adjustment", label: "Reject", onClick: () => ask({ title: `Reject ${a.adjustmentNumber}`, name: "reject-adjustment", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideAdjustment(a.id, a.version, "reject", v.reason), "Adjustment rejected") }) });
                }
                if (a.status === "posted") {
                  acts.push({ id: "reverse-adjustment", label: "Reverse", onClick: () => ask({ title: `Reverse ${a.adjustmentNumber}`, name: "reverse-adjustment", submitLabel: "Reverse", fields: [REASON], onSubmit: (v) => submit(() => api.reverseAdjustment(a.id, a.version, v.reason!), "Adjustment reversed") }) });
                  acts.push({ id: "correct-adjustment", label: "Correct", onClick: () => ask({ title: `Correct ${a.adjustmentNumber}`, name: "correct-adjustment", submitLabel: "Create successor", fields: [{ name: "amount", label: "Correct amount (₹)", required: true, initial: a.amount }, REASON], onSubmit: (v) => submit(() => api.correctAdjustment(a.id, a.version, { amount: v.amount!, reason: v.reason! }), (r) => `${r.adjustmentNumber} drafted as the corrected successor`) }) });
                }
                return <Actions actions={acts} />;
              },
            },
          ]} />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================ Financial aid

export function AidAdminTab({ institution }: { institution: Institution | null }) {
  const api = useChargesApi();
  const { run, submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const iid = institution?.id;
  const decisions = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "aid-decisions"), queryFn: () => api.aidDecisions(iid!), enabled: !!iid });
  const allocations = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "aid-allocations"), queryFn: () => api.aidAllocations(iid!), enabled: !!iid });
  const recs = useQuery({ queryKey: fkeys.scoped(iid ?? "-", "aid-recs"), queryFn: () => api.reconciliations(iid!), enabled: !!iid });
  const charges = useQuery({ queryKey: fkeys.scoped(`${iid}:`, "charges"), queryFn: () => api.charges(iid!), enabled: !!iid });
  if (!institution) return <NoInstitutionTile />;
  return (
    <>
      <BentoTile span={12} title="Handed-off scholarship decisions" data-slice="5A" data-aid-decisions>
        <DataTable caption="Scholarship decisions" rows={decisions.data ?? []} rowKey={(d) => d.eligibilityId}
          empty={<EmptyState icon={GraduationCap} title="No decisions handed off">Student records hands approved scholarship decisions to Financial Aid.</EmptyState>}
          columns={[
            { key: "s", header: "Scholarship", render: (d) => <span data-aid-decision={d.scholarshipCode}>{`${d.scholarshipCode} · ${d.scholarshipName}`}</span> },
            { key: "b", header: "Benefit", render: (d) => `${words(d.benefitType)}${d.benefitValue ? ` · ${d.benefitType === "fee_percentage" ? `${d.benefitValue}%` : formatInr(d.benefitValue)}` : ""}` },
            { key: "v", header: "Valid until", render: (d) => fmtDate(d.validUntil) },
            { key: "st", header: "Decision", render: (d) => <StatusBadge status={d.status} /> },
            { key: "a", header: "Allocation", render: (d) => d.allocationStatus ? <StatusBadge status={d.allocationStatus} /> : d.live ? <Actions actions={[{ id: "allocate-aid", label: "Allocate", variant: "primary", onClick: () => run(() => api.allocate(institution.id, d.eligibilityId), (a) => `${a.allocationNumber}: ${statusLabel(a.status)}`) }]} /> : "—" },
          ]} />
      </BentoTile>
      <BentoTile span={8} title="Aid allocations" data-aid-allocations>
        <DataTable caption="Aid allocations" rows={allocations.data ?? []} rowKey={(a) => a.id}
          empty={<EmptyState icon={GraduationCap} title="No allocations">Allocate a handed-off decision to start.</EmptyState>}
          columns={[
            { key: "n", header: "Allocation", render: (a) => <span data-aid-allocation={a.allocationNumber}>{`${a.allocationNumber} · ${a.scholarshipCode}`}</span> },
            { key: "p", header: "Student", render: (a) => a.displayName },
            { key: "c", header: "Ceiling", render: (a) => (a.allocatedAmount ? formatInr(a.allocatedAmount) : words(a.benefitType)) },
            { key: "u", header: "Applied / paid", render: (a) => `${formatInr(a.appliedAmount)} / ${formatInr(a.disbursedAmount)}` },
            { key: "s", header: "Status", render: (a) => <span data-aid-status={a.allocationNumber}><StatusBadge status={a.status} /></span> },
            {
              key: "x",
              header: "",
              render: (a) => {
                const acts: RowAction[] = [];
                if (a.status === "proposed") acts.push({ id: "approve-aid", label: "Approve", variant: "primary", onClick: () => run(() => api.approveAid(a.id, a.version, "approve"), "Aid allocated") });
                if (a.status === "allocated" || a.status === "disbursed") {
                  const options = (charges.data?.items ?? []).filter((c) => c.studentAccountId === a.studentAccountId && ["published", "active", "disputed"].includes(c.status)).map((c) => ({ value: c.id, label: `${c.chargeNumber} · ${formatInr(c.outstandingAmount)}` }));
                  acts.push({ id: "apply-aid", label: "Apply to charge", variant: "primary", onClick: () => ask({ title: `Apply ${a.allocationNumber}`, name: "apply-aid", submitLabel: "Apply", fields: [{ name: "chargeId", label: "Charge", type: "select", options, required: true, initial: options[0]?.value, wide: true }, { name: "amount", label: "Amount (₹, blank = computed)" }], onSubmit: (v) => submit(() => api.disburse(a.id, a.version, { mode: "charge_adjustment", chargeId: v.chargeId, amount: v.amount || undefined }), "Aid applied") }) });
                  acts.push({ id: "renew-aid", label: "Renew", onClick: () => run(() => api.renewAid(a.id, a.version), "Aid renewed") });
                  acts.push({ id: "revoke-aid", label: "Revoke", onClick: () => ask({ title: `Revoke ${a.allocationNumber}`, name: "revoke-aid", submitLabel: "Revoke and recover", fields: [REASON], onSubmit: (v) => submit(() => api.revokeAid(a.id, a.version, { reason: v.reason!, recover: true }), "Aid revoked") }) });
                }
                return <Actions actions={acts} />;
              },
            },
          ]} />
      </BentoTile>
      <BentoTile span={4} title="Programme reconciliation" data-aid-reconciliation action={<Button size="sm" variant="ghost" data-action="reconcile-aid" onClick={() => run(() => api.reconcile(institution.id), (r) => `Reconciliation: ${statusLabel(r.status)}`)}>Run</Button>}>
        {(recs.data ?? []).length === 0 ? <p>No reconciliation run yet.</p> : (
          <ul>{(recs.data ?? []).slice(0, 5).map((r) => (
            <li key={r.id} data-reconciliation={r.status}>
              <StatusBadge status={r.status} />{` utilised ${formatInr(r.utilisedTotal)} · ledger ${formatInr(r.ledgerTotal)}`}
              {r.exceptions.length > 0 && <small>{` — ${r.exceptions.map((e) => words(e.issue)).join(", ")}`}</small>}
            </li>
          ))}</ul>
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================ Student Fees

export function StudentChargesPanel() {
  const api = useChargesApi();
  const { submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const mine = useQuery({ queryKey: fkeys.scoped("me", "charges"), queryFn: () => api.myCharges() });
  const aid = useQuery({ queryKey: fkeys.scoped("me", "aid"), queryFn: () => api.myAid() });
  const items: ChargeDetail[] = mine.data ?? [];
  return (
    <BentoTile span={8} title="My fees" data-slice="5A" data-my-fees>
      {items.length === 0 ? (
        <EmptyState icon={BadgeIndianRupee} title="No fee charges">Charges appear here once the finance office publishes them.</EmptyState>
      ) : (
        items.map((c) => (
          <article key={c.id} className="sb-card" data-my-charge={c.chargeNumber} style={{ padding: "0.75rem", borderRadius: "0.75rem", border: "1px solid var(--sb-border, #ddd)", marginBottom: "0.75rem" }}>
            <header style={{ display: "flex", justifyContent: "space-between", gap: "0.5rem" }}>
              <strong>{`${c.chargeNumber} · ${c.description}`}</strong>
              <StatusBadge status={c.status} />
            </header>
            <p>
              {`Total ${formatInr(c.totalAmount)}`}
              {c.adjustmentAmount !== "0.00" && ` · concessions / adjustments ${formatInr(c.adjustmentAmount)}`}
              {` · paid ${formatInr(c.settledAmount)} · `}
              <strong data-my-outstanding={c.chargeNumber}>{`due ${formatInr(c.outstandingAmount)}`}</strong>
              {c.dueDate && ` by ${fmtDate(c.dueDate)}`}
            </p>
            {c.instalments.length > 0 && (
              <ul data-my-instalments>{c.instalments.map((i) => <li key={i.id}>{`Instalment ${i.sequence}: ${formatInr(i.amount)} due ${fmtDate(i.dueDate)} `}<StatusBadge status={i.status} /></li>)}</ul>
            )}
            {(c.status === "published" || c.status === "active") && (
              <Actions actions={[{ id: "dispute-charge", label: "Dispute", onClick: () => ask({ title: `Dispute ${c.chargeNumber}`, name: "dispute", submitLabel: "Raise dispute", fields: [REASON], onSubmit: (v) => submit(() => api.raiseDispute(c.id, v.reason!), "Dispute raised — the finance office will review it") }) }]} />
            )}
          </article>
        ))
      )}
      <h3>Scholarships and aid</h3>
      {(aid.data ?? []).length === 0 ? <p data-my-aid="none">No financial aid allocated.</p> : (
        <ul data-my-aid="list">{(aid.data ?? []).map((a) => <li key={a.id}>{`${a.scholarshipName}: applied ${formatInr(a.appliedAmount)}${a.disbursedAmount !== "0.00" ? `, paid ${formatInr(a.disbursedAmount)}` : ""} `}<StatusBadge status={a.status} /></li>)}</ul>
      )}
      {prompt}
      {dialog}
    </BentoTile>
  );
}
