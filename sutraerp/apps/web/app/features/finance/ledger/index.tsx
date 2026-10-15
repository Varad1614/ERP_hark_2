/**
 * Slice **5C** web components (general ledger, journals, periods, treasury). The export names are
 * imported by the route frame (docs/batch5-contracts.md §10); real screens carry
 * `data-slice="5C"`. Money is always a decimal string; ledger screens keep the paise
 * (`formatINR(v, { paise: "always" })` → `ledger`).
 *
 * - `LedgerAdminTab` — Admin → Finance → Ledger (chart of accounts, manual journals with
 *   approval, journal entries and reversal, trial balance, receivables reconciliation).
 * - `PeriodsAdminTab` — Admin → Finance → Periods (financial years and periods: open, lock,
 *   close, reopen, archive).
 * - `TreasuryAdminTab` — Admin → Finance → Bank & treasury (bank accounts, cash position,
 *   statement import and reconciliation, bank transactions).
 */
import { BookOpen, CalendarRange, Inbox, Landmark, Scale } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormSelect, KeyTag, KeyValue, formatDate } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { REASON, todayIso, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { ledger, rowActions, useFinanceAsk } from "../ui";
import { lkeys, useLedgerApi } from "./hooks";
import type { FinancialYear, ManualJournal, Period } from "./types";

export { LedgerApiProvider } from "./hooks";

const ACCOUNT_TYPES = ["asset", "liability", "equity", "income", "expense"].map((t) => ({ value: t, label: statusLabel(t) }));

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="5C">
      <EmptyState icon={Inbox} title="No institution selected">
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

// ------------------------------------------------------------------ Ledger

export function LedgerAdminTab({ institution }: { institution: Institution | null }) {
  const api = useLedgerApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const inst = institution?.id ?? "-";
  const accounts = useQuery({ queryKey: lkeys.scoped(inst, "accounts"), queryFn: () => api.accounts(inst), enabled: !!institution });
  const journals = useQuery({ queryKey: lkeys.scoped(inst, "journals"), queryFn: () => api.journals(inst), enabled: !!institution });
  const entries = useQuery({ queryKey: lkeys.scoped(inst, "entries"), queryFn: () => api.entries(inst), enabled: !!institution });
  const tb = useQuery({ queryKey: lkeys.scoped(inst, "trial-balance"), queryFn: () => api.trialBalance(inst), enabled: !!institution });
  const recs = useQuery({ queryKey: lkeys.scoped(inst, "reconciliations"), queryFn: () => api.reconciliations(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="General ledger" />;
  const accountOptions = [{ value: "", label: "Choose an account" }, ...(accounts.data ?? []).filter((a) => a.status === "active" && a.isPostable).map((a) => ({ value: a.id, label: `${a.code} · ${a.name}` }))];
  const journalActions = (j: ManualJournal): RowAction[] => {
    const a: RowAction[] = [];
    if (j.status === "draft") a.push({ id: "journal-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.submitJournal(j.id, j.version), (r) => `${r.journalNumber} ${statusLabel(r.status).toLowerCase()}`) });
    if (j.status === "pending_approval") {
      a.push({ id: "journal-approve", label: "Approve", onClick: () => void run(() => api.decideJournal(j.id, j.version, true), "Journal approved and posted") });
      a.push({ id: "journal-reject", label: "Reject", onClick: () => ask({ title: `Reject ${j.journalNumber}`, name: "journal-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideJournal(j.id, j.version, false, v.reason), "Journal rejected") }) });
    }
    if (j.status === "posted" && j.journalEntryId) {
      const entry = j.journalEntryId;
      a.push({ id: "journal-reverse", label: "Reverse", onClick: () => ask({ title: `Reverse ${j.journalNumber}`, name: "journal-reverse", fields: [REASON], submitLabel: "Post reversal", onSubmit: (v) => submit(() => api.reverseEntry(entry, v.reason!), (e) => `Reversal ${e.entryNumber} posted`) }) });
    }
    return a;
  };
  const last = recs.data?.[0];
  return (
    <>
      <BentoTile
        span={7}
        title="Manual journals"
        data-slice="5C"
        data-manual-journals
        action={
          <Button
            size="sm"
            icon={<BookOpen strokeWidth={1.75} />}
            data-new-journal
            onClick={() =>
              ask({
                title: "New manual journal",
                name: "manual-journal",
                submitLabel: "Save draft",
                fields: [
                  { name: "postingDate", label: "Posting date", type: "date", required: true, initial: todayIso() },
                  { name: "description", label: "Description", required: true, wide: true },
                  { name: "debit", label: "Debit account", type: "select", required: true, options: accountOptions, wide: true },
                  { name: "credit", label: "Credit account", type: "select", required: true, options: accountOptions, wide: true },
                  { name: "amount", label: "Amount (₹)", required: true, placeholder: "500.00" },
                  { name: "memo", label: "Memo / bank reference" },
                ],
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.createJournal(inst, {
                        postingDate: v.postingDate!,
                        description: v.description!,
                        lines: [
                          { ledgerAccountId: v.debit!, debit: v.amount!, memo: v.memo || undefined },
                          { ledgerAccountId: v.credit!, credit: v.amount!, memo: v.memo || undefined },
                        ],
                      }),
                    (j) => `${j.journalNumber} saved as draft`,
                  ),
              })
            }
          >
            New journal
          </Button>
        }
      >
        <DataTable
          caption="Manual journals"
          rows={journals.data ?? []}
          rowKey={(j) => j.id}
          empty={<EmptyState icon={BookOpen} title="No manual journals">Corrections, accruals and reclassifications are posted here after approval.</EmptyState>}
          columns={[
            { key: "n", header: "Journal", render: (j) => <span data-journal={j.journalNumber}>{j.journalNumber}</span> },
            { key: "d", header: "Date", render: (j) => formatDate(j.postingDate) },
            { key: "t", header: "Description", render: (j) => j.description },
            { key: "a", header: "Amount", render: (j) => ledger(j.totalAmount) },
            { key: "s", header: "Status", render: (j) => <StatusBadge status={j.status} /> },
            { key: "x", header: "", render: (j) => rowActions(journalActions(j), busy) },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={5}
        title="Receivables reconciliation"
        data-reconciliation
        action={
          <Button size="sm" variant="ghost" data-run-reconciliation onClick={() => void run(() => api.reconcile(inst), (r) => `Reconciliation ${statusLabel(r.status).toLowerCase()}`)} disabled={busy}>
            Run
          </Button>
        }
      >
        {last ? (
          <p data-reconciliation-status={last.status}>
            <StatusBadge status={last.status} /> Charges {ledger(last.sourceTotal)} · ledger {ledger(last.ledgerTotal)} · difference {ledger(last.difference)}
            {last.exceptions.length > 0 ? ` · ${last.exceptions.length} charge(s) to investigate` : ""}
          </p>
        ) : (
          <EmptyState icon={Scale} title="Not reconciled yet">Compare charge balances with the receivable control account.</EmptyState>
        )}
      </BentoTile>
      <BentoTile
        span={12}
        title="Trial balance"
        data-trial-balance
        action={tb.data ? <Badge kind={tb.data.balanced ? "done" : "attention"}>{tb.data.balanced ? "Balanced" : "Out of balance"}</Badge> : undefined}
      >
        {tb.data && tb.data.rows.length > 0 ? (
          <>
            <p>
              As of {formatDate(tb.data.asOf)} · debits <b data-tb-debit>{ledger(tb.data.totalDebit)}</b> · credits <b data-tb-credit>{ledger(tb.data.totalCredit)}</b>
            </p>
            <DataTable
              caption="Trial balance"
              rows={tb.data.rows}
              rowKey={(r) => r.ledgerAccountId}
              columns={[
                { key: "c", header: "Account", render: (r) => <>{r.name} <KeyTag>{r.code}</KeyTag></> },
                { key: "d", header: "Debit", render: (r) => ledger(r.debit) },
                { key: "k", header: "Credit", render: (r) => ledger(r.credit) },
                { key: "b", header: "Balance", render: (r) => ledger(r.balance) },
              ]}
            />
          </>
        ) : (
          <EmptyState icon={Scale} title="Nothing posted yet">Postings from charges, payments and journals appear here.</EmptyState>
        )}
      </BentoTile>
      <BentoTile span={12} title="Journal entries" data-journal-entries>
        <DataTable
          caption="Journal entries"
          rows={entries.data?.items ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState icon={BookOpen} title="The ledger is empty">Every posting — charge publication, payment, refund, journal — lands here.</EmptyState>}
          columns={[
            { key: "n", header: "Entry", render: (e) => <span data-entry={e.entryNumber}>{e.entryNumber}</span> },
            { key: "d", header: "Date", render: (e) => formatDate(e.postingDate) },
            { key: "s", header: "Source", render: (e) => statusLabel(e.sourceType) },
            { key: "t", header: "Description", render: (e) => e.description },
            { key: "a", header: "Amount", render: (e) => ledger(e.totalDebit) },
            { key: "k", header: "Kind", render: (e) => (e.reversedByEntryId ? <Badge kind="neutral">Reversed</Badge> : statusLabel(e.kind)) },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Chart of accounts"
        data-chart
        action={
          <Button
            size="sm"
            variant="ghost"
            data-new-account
            onClick={() =>
              ask({
                title: "Add a ledger account",
                name: "ledger-account",
                submitLabel: "Add account",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "5950" },
                  { name: "name", label: "Name", required: true, wide: true },
                  { name: "accountType", label: "Type", type: "select", required: true, options: ACCOUNT_TYPES, initial: "expense" },
                ],
                onSubmit: (v) => submit(() => api.createAccount(inst, { code: v.code!, name: v.name!, accountType: v.accountType! }), (a) => `Account ${a.code} added`),
              })
            }
          >
            Add account
          </Button>
        }
      >
        <DataTable
          caption="Chart of accounts"
          rows={accounts.data ?? []}
          rowKey={(a) => a.id}
          columns={[
            { key: "c", header: "Code", render: (a) => <span data-account={a.code}>{a.code}</span> },
            { key: "n", header: "Name", render: (a) => a.name },
            { key: "t", header: "Type", render: (a) => statusLabel(a.accountType) },
            { key: "b", header: "Balance", render: (a) => ledger(a.balance) },
            { key: "s", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Periods

function periodActions(p: Period, year: FinancialYear, cmd: (p: Period, c: "open" | "lock" | "close" | "reopen" | "archive", reason?: string) => void, askReason: (p: Period) => void): RowAction[] {
  const a: RowAction[] = [];
  const yearOpen = year.status === "open";
  if ((p.status === "planned" || p.status === "locked") && yearOpen) a.push({ id: "period-open", label: p.status === "locked" ? "Unlock" : "Open", onClick: () => cmd(p, "open") });
  if (p.status === "open" || p.status === "reopened") a.push({ id: "period-lock", label: "Lock", onClick: () => cmd(p, "lock") });
  if (p.status === "open" || p.status === "locked" || p.status === "reopened") a.push({ id: "period-close", label: "Close", variant: "primary", onClick: () => cmd(p, "close") });
  if (p.status === "closed" && yearOpen) a.push({ id: "period-reopen", label: "Reopen", onClick: () => askReason(p) });
  if (p.status === "closed") a.push({ id: "period-archive", label: "Archive", onClick: () => cmd(p, "archive") });
  return a;
}

export function PeriodsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useLedgerApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const inst = institution?.id ?? "-";
  const years = useQuery({ queryKey: lkeys.scoped(inst, "years"), queryFn: () => api.years(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="Financial periods" />;
  const cmd = (p: Period, c: "open" | "lock" | "close" | "reopen" | "archive", reason?: string) => void run(() => api.periodCommand(p.id, p.version, c, reason), (r) => `${r.name} ${statusLabel(r.status).toLowerCase()}`);
  const askReason = (p: Period) =>
    ask({ title: `Reopen ${p.name}`, name: "period-reopen", fields: [REASON], submitLabel: "Reopen period", onSubmit: (v) => submit(() => api.periodCommand(p.id, p.version, "reopen", v.reason), `${p.name} reopened`) });
  const list = years.data ?? [];
  return (
    <>
      <BentoTile
        span={12}
        title="Financial years"
        data-slice="5C"
        data-years
        action={
          <Button
            size="sm"
            icon={<CalendarRange strokeWidth={1.75} />}
            data-new-year
            onClick={() =>
              ask({
                title: "Establish a financial year",
                name: "financial-year",
                submitLabel: "Establish year",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "FY2026-27" },
                  { name: "name", label: "Name", required: true, placeholder: "Financial year 2026-27" },
                  { name: "startDate", label: "Starts", type: "date", required: true, initial: "2026-04-01" },
                  { name: "endDate", label: "Ends", type: "date", required: true, initial: "2027-03-31" },
                ],
                onSubmit: (v) => submit(() => api.createYear(inst, { code: v.code!, name: v.name!, startDate: v.startDate!, endDate: v.endDate! }), (y) => `${y.code} established with ${y.periods.length} periods`),
              })
            }
          >
            Establish year
          </Button>
        }
      >
        {list.length === 0 ? (
          <EmptyState icon={CalendarRange} title="No financial year yet">Until a year is established every posting date is accepted.</EmptyState>
        ) : (
          <DataTable
            caption="Financial years"
            rows={list}
            rowKey={(y) => y.id}
            columns={[
              { key: "c", header: "Year", render: (y) => <span data-year={y.code}>{y.code}</span> },
              { key: "n", header: "Name", render: (y) => y.name },
              { key: "d", header: "Dates", render: (y) => `${formatDate(y.startDate)} – ${formatDate(y.endDate)}` },
              { key: "s", header: "Status", render: (y) => <StatusBadge status={y.status} /> },
              {
                key: "x",
                header: "",
                render: (y) =>
                  rowActions(
                    [
                      ...(y.status === "draft" ? [{ id: "year-open", label: "Open year", onClick: () => void run(() => api.yearCommand(y.id, y.version, "open"), `${y.code} opened`) }] : []),
                      ...(y.status === "open" ? [{ id: "year-close", label: "Close year", onClick: () => void run(() => api.yearCommand(y.id, y.version, "close"), `${y.code} closed`) }] : []),
                      ...(y.status === "closed" ? [{ id: "year-archive", label: "Archive", onClick: () => void run(() => api.yearCommand(y.id, y.version, "archive"), `${y.code} archived`) }] : []),
                    ],
                    busy,
                  ),
              },
            ]}
          />
        )}
      </BentoTile>
      {list.map((y) => (
        <BentoTile key={y.id} span={12} title={`Periods · ${y.code}`} data-periods={y.code}>
          <DataTable
            caption={`Periods of ${y.code}`}
            rows={y.periods}
            rowKey={(p) => p.id}
            columns={[
              { key: "n", header: "Period", render: (p) => <span data-period={p.name}>{p.name}</span> },
              { key: "d", header: "Dates", render: (p) => `${formatDate(p.startDate)} – ${formatDate(p.endDate)}` },
              { key: "e", header: "Entries", render: (p) => String(p.entryCount) },
              { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
              { key: "p", header: "Postings", render: (p) => (p.acceptsPostings ? "Accepted" : "Refused") },
              { key: "x", header: "", render: (p) => rowActions(periodActions(p, y, cmd, askReason), busy) },
            ]}
          />
        </BentoTile>
      ))}
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Treasury

export function TreasuryAdminTab({ institution }: { institution: Institution | null }) {
  const api = useLedgerApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useFinanceAsk();
  const inst = institution?.id ?? "-";
  const [bankId, setBankId] = useState("");
  const [statementId, setStatementId] = useState("");
  const banks = useQuery({ queryKey: lkeys.scoped(inst, "banks"), queryFn: () => api.bankAccounts(inst), enabled: !!institution });
  const cash = useQuery({ queryKey: lkeys.scoped(inst, "cash"), queryFn: () => api.cashPosition(inst), enabled: !!institution });
  const txns = useQuery({ queryKey: lkeys.scoped(inst, "transactions"), queryFn: () => api.transactions(inst), enabled: !!institution });
  const bank = bankId || banks.data?.find((b) => b.status === "active")?.id || "";
  const statements = useQuery({ queryKey: lkeys.scoped(bank || "-", "statements"), queryFn: () => api.statements(bank), enabled: !!institution && !!bank });
  const current = statementId || statements.data?.[0]?.id || "";
  const statement = useQuery({ queryKey: lkeys.scoped(current || "-", "statement"), queryFn: () => api.statement(current), enabled: !!current });
  if (!institution) return <NoInstitutionTile title="Bank & treasury" />;
  return (
    <>
      <BentoTile
        span={8}
        title="Bank accounts"
        data-slice="5C"
        data-bank-accounts
        action={
          <Button
            size="sm"
            icon={<Landmark strokeWidth={1.75} />}
            data-new-bank-account
            onClick={() =>
              ask({
                title: "Add a bank account",
                name: "bank-account",
                submitLabel: "Request account",
                fields: [
                  { name: "name", label: "Name", required: true, placeholder: "Main collections" },
                  { name: "bankName", label: "Bank", required: true, placeholder: "HDFC Bank" },
                  { name: "accountNumberLast4", label: "Account number (last 4)", required: true },
                  { name: "ifsc", label: "IFSC", required: true, placeholder: "HDFC0001234" },
                ],
                onSubmit: (v) => submit(() => api.createBankAccount(inst, { name: v.name!, bankName: v.bankName!, accountNumberLast4: v.accountNumberLast4!, ifsc: v.ifsc! }), (b) => `${b.name} ${statusLabel(b.status).toLowerCase()}`),
              })
            }
          >
            Add bank account
          </Button>
        }
      >
        <DataTable
          caption="Bank accounts"
          rows={banks.data ?? []}
          rowKey={(b) => b.id}
          empty={<EmptyState icon={Landmark} title="No bank accounts">Add the collection account the gateway settles into.</EmptyState>}
          columns={[
            { key: "n", header: "Account", render: (b) => <span data-bank={b.name}>{b.name}</span> },
            { key: "b", header: "Bank", render: (b) => `${b.bankName} ··${b.accountNumberLast4}` },
            { key: "l", header: "Ledger", render: (b) => <KeyTag>{b.ledgerAccountCode}</KeyTag> },
            { key: "a", header: "Balance", render: (b) => ledger(b.ledgerBalance) },
            { key: "u", header: "Unreconciled", render: (b) => b.unreconciledLines.toLocaleString("en-IN") },
            { key: "s", header: "Status", render: (b) => <StatusBadge status={b.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={4} title="Cash position" data-cash-position>
        {cash.data ? (
          <>
            <p>
              Available <b data-cash-available>{ledger(cash.data.available)}</b> · in transit {ledger(cash.data.inTransit)}
            </p>
            <ul style={{ listStyle: "none", margin: 0, padding: 0 }}>
              {cash.data.rows.map((r) => (
                <li key={`${r.kind}-${r.ledgerAccountCode}`} data-cash-row={r.ledgerAccountCode}>
                  <KeyValue label={<>{r.name} <KeyTag>{r.ledgerAccountCode}</KeyTag></>} value={<span data-cash-balance>{ledger(r.balance)}</span>} />
                </li>
              ))}
            </ul>
          </>
        ) : (
          <EmptyState icon={Scale} title="No position yet">Balances come straight from the ledger.</EmptyState>
        )}
      </BentoTile>
      <BentoTile
        span={12}
        title="Bank statements"
        data-statements
        action={
          <div className="sb-row-actions">
            <div style={{ minWidth: 200 }}>
              <FormSelect
                label="Bank account"
                hideLabel
                value={bank}
                onChange={(e) => {
                  setBankId(e.target.value);
                  setStatementId("");
                }}
                options={[{ value: "", label: "Choose an account" }, ...(banks.data ?? []).map((b) => ({ value: b.id, label: b.name }))]}
              />
            </div>
            <Button
              size="sm"
              data-import-statement
              disabled={!bank}
              onClick={() =>
                ask({
                  title: "Import a bank statement",
                  name: "statement-import",
                  submitLabel: "Import",
                  fields: [
                    { name: "statementRef", label: "Statement reference", required: true, placeholder: "JUL-2026" },
                    { name: "periodFrom", label: "From", type: "date", required: true },
                    { name: "periodTo", label: "To", type: "date", required: true },
                    { name: "closingBalance", label: "Closing balance (₹)" },
                    { name: "content", label: "Lines (JSON lines: valueDate, direction, amount, reference)", required: true, wide: true },
                  ],
                  onSubmit: (v) =>
                    submit(
                      () => api.importStatement(bank, { statementRef: v.statementRef!, periodFrom: v.periodFrom!, periodTo: v.periodTo!, closingBalance: v.closingBalance || undefined, content: v.content! }),
                      (s) => `${s.statementRef} imported · ${s.lineCount} lines`,
                    ),
                })
              }
            >
              Import statement
            </Button>
          </div>
        }
      >
        <DataTable
          caption="Statements"
          rows={statements.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={Inbox} title="No statements">Import the bank's statement to reconcile it with the ledger.</EmptyState>}
          columns={[
            { key: "r", header: "Statement", render: (s) => <span data-statement={s.statementRef}>{s.statementRef}</span> },
            { key: "p", header: "Period", render: (s) => `${formatDate(s.periodFrom)} – ${formatDate(s.periodTo)}` },
            { key: "c", header: "Credits", render: (s) => ledger(s.totalCredits) },
            { key: "d", header: "Debits", render: (s) => ledger(s.totalDebits) },
            { key: "s", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "x",
              header: "",
              render: (s) =>
                rowActions(
                  [
                    { id: "statement-view", label: "Lines", onClick: () => setStatementId(s.id) },
                    ...(s.status !== "reconciled" ? [{ id: "statement-reconcile", label: "Reconcile", variant: "primary" as const, onClick: () => void run(() => api.reconcileStatement(s.id, s.version), (r) => `${r.statementRef} ${statusLabel(r.status).toLowerCase()}`) }] : []),
                  ],
                  busy,
                ),
            },
          ]}
        />
        {statement.data && (
          <DataTable
            caption={`Lines of ${statement.data.statementRef}`}
            rows={statement.data.lines}
            rowKey={(l) => l.id}
            columns={[
              { key: "n", header: "#", render: (l) => String(l.lineNo) },
              { key: "d", header: "Date", render: (l) => formatDate(l.valueDate) },
              { key: "r", header: "Reference", render: (l) => l.reference ?? l.description ?? "—" },
              { key: "a", header: "Amount", render: (l) => ledger(l.direction === "debit" ? `-${l.amount}` : l.amount) },
              { key: "m", header: "Match", render: (l) => <span data-line-match={l.matchStatus}>{l.matchedEntryNumber ?? (l.matchedTransactionId ? "Settlement" : statusLabel(l.matchStatus))}</span> },
            ]}
          />
        )}
      </BentoTile>
      <BentoTile span={12} title="Bank transactions" data-bank-transactions>
        <DataTable
          caption="Bank transactions"
          rows={txns.data ?? []}
          rowKey={(t) => t.id}
          empty={<EmptyState icon={Landmark} title="No bank transactions">Gateway settlements to the bank appear here.</EmptyState>}
          columns={[
            { key: "r", header: "Reference", render: (t) => t.reference },
            { key: "d", header: "Value date", render: (t) => formatDate(t.valueDate) },
            { key: "a", header: "Net", render: (t) => ledger(t.amount) },
            { key: "f", header: "Fees", render: (t) => ledger(t.feeAmount) },
            { key: "n", header: "Payments", render: (t) => String(t.items.length) },
            { key: "s", header: "Status", render: (t) => <StatusBadge status={t.status} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
