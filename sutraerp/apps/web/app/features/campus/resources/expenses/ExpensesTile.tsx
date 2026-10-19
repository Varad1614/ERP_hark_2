/**
 * ERP_EXTENSIONS 3D: staff expense claims, finance side (Admin → Campus → Procurement). Overview
 * shows what waits on approval and payment, spend by category and the six-month trend; Approvals
 * opens a submitted claim with its receipts beside what OCR read off them; Reimburse records the
 * payment (date + UTR); Settings holds the receipt threshold, the age limit and the categories.
 */
import { Receipt } from "lucide-react";
import { Badge, BarChart, BentoTile, Button, DataTable, Drawer, EmptyState, formatDate, FormGrid, KeyValue, TextArea, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../../lib/api";
import { useCommand } from "../../../institution/hooks";
import { createHttpExpensesApi, type ExpensesApi } from "./api";
import { flagLabel, inr, isBlocking, money, monthLabel, ocrMismatch, STATUS_LABEL } from "./expenses";
import { createMockExpensesApi } from "./mock";
import type { ExpenseCategory, ExpenseClaimView, ExpenseSettings, ExpenseView } from "./types";

let fallback: ExpensesApi | null = null;
export function defaultExpensesApi(): ExpensesApi {
  fallback ??= USES_API ? createHttpExpensesApi() : createMockExpensesApi();
  return fallback;
}

type Tab = "overview" | "approvals" | "reimburse" | "settings";
const TABS: { value: Tab; label: string }[] = [
  { value: "overview", label: "Overview" },
  { value: "approvals", label: "Approvals" },
  { value: "reimburse", label: "Reimburse" },
  { value: "settings", label: "Settings" },
];

const today = () => new Date().toISOString().slice(0, 10);
const when = (ts: string | null) => (ts ? new Date(ts).toLocaleDateString("en-IN", { day: "2-digit", month: "short", year: "numeric" }) : "—");

/** Policy chips of a line or claim. */
export function FlagChips({ flags, maxAgeDays }: { flags: string[]; maxAgeDays?: number }) {
  if (!flags.length) return <Badge kind="done">OK</Badge>;
  return (
    <span className="flex flex-wrap gap-1" data-flags={flags.join(" ")}>
      {flags.map((f) => (
        <Badge key={f} kind={isBlocking(f) ? "attention" : "review"}>{flagLabel(f, maxAgeDays)}</Badge>
      ))}
    </span>
  );
}

/** Small receipt preview that opens the file in a new tab. */
export function ReceiptThumb({ api, receiptId }: { api: ExpensesApi; receiptId: string | null }) {
  if (!receiptId) return <span>No receipt</span>;
  const url = api.receiptUrl(receiptId);
  return (
    <a href={url} target="_blank" rel="noreferrer" data-receipt={receiptId} title="Open the receipt">
      <img src={url} alt="Receipt" loading="lazy" style={{ width: 48, height: 64, objectFit: "cover", borderRadius: 4, border: "1px solid var(--sb-line, #ddd)" }} />
    </a>
  );
}

/** "Receipt said ₹X" next to the claimed amount. */
export function OcrCompare({ line }: { line: ExpenseView }) {
  const read = line.ocr?.total;
  if (typeof read !== "number") return <span>{money(line.amount, line.currency)}</span>;
  const diff = ocrMismatch(line);
  return (
    <span data-ocr-compare={diff ? "mismatch" : "match"}>
      {money(line.amount, line.currency)}
      <br />
      <small>Receipt said {inr(read)}</small> {diff && <Badge kind="attention">Differs by {inr(Math.abs(diff.claimed - diff.read))}</Badge>}
    </span>
  );
}

export function ClaimLinesTable({ api, claim, maxAgeDays }: { api: ExpensesApi; claim: ExpenseClaimView; maxAgeDays?: number }) {
  return (
    <DataTable caption="Lines" rows={claim.lines} rowKey={(l) => l.id}
      empty={<EmptyState icon={Receipt} title="No lines" compact>This claim has no expenses.</EmptyState>}
      columns={[
        { key: "r", header: "Receipt", render: (l) => <ReceiptThumb api={api} receiptId={l.receiptId} /> },
        { key: "d", header: "Date", render: (l) => formatDate(l.spentOn) },
        { key: "m", header: "Merchant", render: (l) => l.merchant },
        { key: "c", header: "Category", render: (l) => l.categoryName ?? "—" },
        { key: "a", header: "Amount", render: (l) => <OcrCompare line={l} /> },
        { key: "t", header: "GST", render: (l) => (l.tax ? inr(l.tax) : "—"), muted: true },
        { key: "f", header: "Checks", render: (l) => <FlagChips flags={l.flags} maxAgeDays={maxAgeDays} /> },
      ]} />
  );
}

export function ExpensesTile({ institutionId, api = defaultExpensesApi() }: { institutionId: string; api?: ExpensesApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("overview");
  const [openId, setOpenId] = useState("");
  const [reason, setReason] = useState("");
  const [pay, setPay] = useState<Record<string, { paidOn: string; reference: string }>>({});
  const [settingsDraft, setSettingsDraft] = useState<ExpenseSettings | null>(null);
  const key = (...p: string[]) => ["institution", "expenses", institutionId, ...p];
  const dash = useQuery({ queryKey: key("dashboard"), queryFn: () => api.dashboard(institutionId), enabled: tab === "overview" });
  const submitted = useQuery({ queryKey: key("claims", "submitted"), queryFn: () => api.claims(institutionId, "submitted"), enabled: tab === "approvals" });
  const approved = useQuery({ queryKey: key("claims", "approved"), queryFn: () => api.claims(institutionId, "approved"), enabled: tab === "reimburse" });
  const one = useQuery({ queryKey: key("claim", openId), queryFn: () => api.claim(openId), enabled: openId !== "" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId) });
  const shown = settingsDraft ?? settings.data ?? null;
  const maxAge = settings.data?.maxAgeDays;
  const c = one.data;

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-expenses-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );
  const close = () => {
    setOpenId("");
    setReason("");
  };
  const setCat = (i: number, patch: Partial<ExpenseCategory>) => {
    if (!shown) return;
    setSettingsDraft({ ...shown, categories: shown.categories.map((x, j) => (j === i ? { ...x, ...patch } : x)) });
  };
  const blankToNull = (s: string) => (s.trim() === "" ? null : s.trim());

  return (
    <BentoTile span={12} title="Expense claims" data-slice="3D" data-expenses action={tabs}>
      {tab === "overview" && dash.data && (
        <>
          <FormGrid columns={3}>
            <KeyValue label="Awaiting approval" value={`${dash.data.awaitingApproval} · ${inr(dash.data.awaitingApprovalAmount)}`} />
            <KeyValue label="Awaiting reimbursement" value={`${dash.data.awaitingReimbursement} · ${inr(dash.data.awaitingReimbursementAmount)}`} />
            <KeyValue label="Receipt mismatches" value={dash.data.ocrMismatches ? `${dash.data.ocrMismatches} line${dash.data.ocrMismatches === 1 ? "" : "s"} differ from the receipt` : "None"} />
          </FormGrid>
          <BarChart label="Approved spend, last six months" height={160}
            data={dash.data.trend.map((m) => ({ label: monthLabel(m.month), value: Number(m.amount), display: inr(m.amount) }))} />
          <DataTable caption="This financial year by category" rows={dash.data.byCategory} rowKey={(r) => r.categoryId ?? r.name}
            empty={<EmptyState icon={Receipt} title="No approved spend yet" compact>Approved claims show here by category.</EmptyState>}
            columns={[
              { key: "n", header: "Category", render: (r) => r.name },
              { key: "a", header: "Approved", render: (r) => inr(r.amount) },
            ]} />
        </>
      )}

      {tab === "approvals" && (
        <DataTable caption="Submitted claims" rows={submitted.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={Receipt} title="Nothing to approve" compact>Submitted claims wait here.</EmptyState>}
          columns={[
            { key: "n", header: "Claim", render: (r) => r.number },
            { key: "s", header: "Staff", render: (r) => r.staffLabel },
            { key: "t", header: "Title", render: (r) => r.title },
            { key: "l", header: "Lines", render: (r) => String(r.lineCount) },
            { key: "a", header: "Total", render: (r) => inr(r.total) },
            { key: "w", header: "Submitted", render: (r) => when(r.submittedAt), muted: true },
            { key: "o", header: "", render: (r) => <Button size="sm" variant="ghost" data-action="open-claim" onClick={() => setOpenId(r.id)}>Review</Button> },
          ]} />
      )}

      {tab === "reimburse" && (
        <DataTable caption="Approved, not yet paid" rows={approved.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={Receipt} title="Nothing to pay" compact>Approved claims wait here for payment.</EmptyState>}
          columns={[
            { key: "n", header: "Claim", render: (r) => r.number },
            { key: "s", header: "Staff", render: (r) => r.staffLabel },
            { key: "a", header: "Amount", render: (r) => inr(r.total) },
            { key: "p", header: "Paid on", render: (r) => (
              <TextField label="Paid on" hideLabel type="date" max={today()} value={pay[r.id]?.paidOn ?? today()}
                onChange={(e) => setPay({ ...pay, [r.id]: { reference: pay[r.id]?.reference ?? "", paidOn: e.target.value } })} />
            ) },
            { key: "u", header: "UTR / reference", render: (r) => (
              <TextField label="UTR / reference" hideLabel placeholder="UTR, cheque or payroll ref" value={pay[r.id]?.reference ?? ""}
                onChange={(e) => setPay({ ...pay, [r.id]: { paidOn: pay[r.id]?.paidOn ?? today(), reference: e.target.value } })} />
            ) },
            { key: "x", header: "", render: (r) => (
              <Button size="sm" data-action="reimburse-claim" disabled={busy}
                onClick={() => void run(() => api.reimburse(r.id, { paidOn: pay[r.id]?.paidOn || today(), reference: blankToNull(pay[r.id]?.reference ?? ""), version: r.version }), `${r.number} marked reimbursed`)}>
                Mark paid
              </Button>
            ) },
          ]} />
      )}

      {tab === "settings" && shown && (
        <>
          <FormGrid columns={2}>
            <TextField label="Receipt needed above (₹)" inputMode="decimal" value={shown.receiptRequiredAbove} onChange={(e) => setSettingsDraft({ ...shown, receiptRequiredAbove: e.target.value })} />
            <TextField label="Oldest expense that can be claimed (days, 0 = no limit)" inputMode="numeric" value={String(shown.maxAgeDays)} onChange={(e) => setSettingsDraft({ ...shown, maxAgeDays: Number(e.target.value) || 0 })} />
          </FormGrid>
          <DataTable caption="Categories" rows={shown.categories.map((cat, i) => ({ cat, i }))} rowKey={(r) => String(r.i)}
            columns={[
              { key: "k", header: "Key", render: ({ cat, i }) => <TextField label="Key" hideLabel value={cat.id} placeholder="local_conveyance" onChange={(e) => setCat(i, { id: e.target.value })} /> },
              { key: "n", header: "Name", render: ({ cat, i }) => <TextField label="Name" hideLabel value={cat.name} onChange={(e) => setCat(i, { name: e.target.value })} /> },
              { key: "l", header: "Limit per expense (₹)", render: ({ cat, i }) => <TextField label="Limit" hideLabel inputMode="decimal" placeholder="No limit" value={cat.limit ?? ""} onChange={(e) => setCat(i, { limit: blankToNull(e.target.value) })} /> },
              { key: "r", header: "Receipt above (₹)", render: ({ cat, i }) => <TextField label="Receipt above" hideLabel inputMode="decimal" placeholder={`Default ${shown.receiptRequiredAbove}`} value={cat.receiptAbove ?? ""} onChange={(e) => setCat(i, { receiptAbove: blankToNull(e.target.value) })} /> },
              { key: "x", header: "", render: ({ i }) => <Button size="sm" variant="ghost" data-action="remove-category" onClick={() => setSettingsDraft({ ...shown, categories: shown.categories.filter((_, j) => j !== i) })}>Remove</Button> },
            ]} />
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" data-action="add-category" onClick={() => setSettingsDraft({ ...shown, categories: [...shown.categories, { id: "", name: "", limit: null, receiptAbove: null }] })}>Add category</Button>
            <Button size="sm" data-action="save-expense-settings" disabled={busy || settingsDraft === null || shown.categories.some((x) => !x.id.trim() || !x.name.trim())}
              onClick={() => void run(async () => {
                const r = await api.saveSettings(institutionId, shown);
                setSettingsDraft(null);
                return r;
              }, "Settings saved")}>Save settings</Button>
          </div>
        </>
      )}

      <Drawer open={openId !== "" && !!c} onClose={close} title={c ? `${c.number} · ${c.title}` : "Claim"} eyebrow={c ? `${c.staffLabel} · ${STATUS_LABEL[c.status] ?? c.status}` : undefined}
        footer={c && c.status === "submitted" ? (
          <div className="flex gap-2">
            <Button size="sm" data-action="approve-claim" disabled={busy}
              onClick={() => void run(async () => {
                const r = await api.decide(c.id, { decision: "approve", version: c.version });
                close();
                return r;
              }, `${c.number} approved`)}>Approve</Button>
            <Button size="sm" variant="ghost" data-action="return-claim" disabled={busy || !reason.trim()}
              onClick={() => void run(async () => {
                const r = await api.decide(c.id, { decision: "return", reason: reason.trim(), version: c.version });
                close();
                return r;
              }, `${c.number} sent back`)}>Send back</Button>
          </div>
        ) : undefined}>
        {c && (
          <div data-claim-detail={c.id}>
            <FormGrid columns={3}>
              <KeyValue label="Total" value={inr(c.total)} />
              <KeyValue label="GST" value={inr(c.taxTotal)} />
              <KeyValue label="Receipts" value={`${c.withReceipt} of ${c.lineCount}`} />
            </FormGrid>
            {c.purpose && <KeyValue label="Purpose" value={c.purpose} />}
            {c.flags.length > 0 && <FlagChips flags={c.flags} maxAgeDays={maxAge} />}
            <ClaimLinesTable api={api} claim={c} maxAgeDays={maxAge} />
            {c.status === "submitted" && (
              <TextArea label="Reason for sending back (shown to the claimant)" value={reason} placeholder="e.g. Attach the bus ticket for 12 Oct" onChange={(e) => setReason(e.target.value)} />
            )}
          </div>
        )}
      </Drawer>
      {dialog}
    </BentoTile>
  );
}
