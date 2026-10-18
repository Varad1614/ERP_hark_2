/**
 * ERP_EXTENSIONS 3D: "My expenses" (staff self-service, My HR → Payslips). Snap a receipt, check
 * what PaddleOCR read off it, save the expense, pick unclaimed expenses into a claim and submit it.
 * Every field the reader fills is a suggestion the claimant confirms.
 */
import { Camera, Receipt } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, DataTable, EmptyState, formatDate, FormGrid, FormSelect, KeyValue, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useCommand } from "../../../institution/hooks";
import type { ExpensesApi } from "./api";
import {
  blankForm, flagLabel, formFromExpense, formFromReading, formReady, inputOfForm, inr, lowConfidence, money, PAYMENT_METHODS, prepareReceipt, STATUS_LABEL, statusKind,
  submitBlockers, type ExpenseForm,
} from "./expenses";
import { defaultExpensesApi, FlagChips, OcrCompare, ReceiptThumb } from "./ExpensesTile";
import type { ExpenseView, ReceiptReading } from "./types";

const today = () => new Date().toISOString().slice(0, 10);

/** Gentle note above the confirm form, from how the receipt was read. */
export function readingNote(reading: ReceiptReading | null, engine: string): { kind: "info" | "attention"; text: string } {
  if (!reading) return { kind: "info", text: "Type the expense in. Attach a receipt when the policy needs one." };
  if (reading.status === "disabled") return { kind: "info", text: "Receipt saved. Reading receipts isn’t set up here yet, so please type the details in." };
  if (reading.status === "failed") return { kind: "info", text: "Receipt saved, but it couldn’t be read this time. Please type the details in." };
  const by = engine === "paddleocr" || reading.engine === "paddleocr" ? "PaddleOCR" : reading.engine || engine;
  return lowConfidence(reading)
    ? { kind: "attention", text: `Read by ${by}, but it wasn’t sure about this one. Check every field carefully against the receipt.` }
    : { kind: "info", text: `Read by ${by} — check every field.` };
}

export function MyExpensesTile({ api = defaultExpensesApi() }: { api?: ExpensesApi }) {
  const { run, busy, dialog } = useCommand();
  const fileRef = useRef<HTMLInputElement>(null);
  const [form, setForm] = useState<ExpenseForm | null>(null);
  const [editing, setEditing] = useState<{ id: string; version: number } | null>(null);
  const [reading, setReading] = useState<ReceiptReading | null>(null);
  const [duplicate, setDuplicate] = useState(false);
  const [picked, setPicked] = useState<string[]>([]);
  const [claimDraft, setClaimDraft] = useState({ title: "", purpose: "" });
  const [openId, setOpenId] = useState("");
  const key = (...p: string[]) => ["institution", "expenses", "me", ...p];
  const mine = useQuery({ queryKey: key("summary"), queryFn: () => api.mine(), retry: false });
  const one = useQuery({ queryKey: key("claim", openId), queryFn: () => api.claim(openId), enabled: openId !== "" });
  const m = mine.data;
  const c = one.data;
  const maxAge = m?.settings.maxAgeDays;
  const categories = (m?.settings.categories ?? []).map((x) => ({ value: x.id, label: x.name }));

  const resetForm = () => {
    setForm(null);
    setEditing(null);
    setReading(null);
    setDuplicate(false);
  };
  const onFile = (file: File | null) => {
    if (!file) return;
    void run(async () => {
      const upload = await prepareReceipt(file);
      const r = await api.uploadReceipt(upload);
      setReading(r.reading);
      setDuplicate(r.duplicate);
      setEditing(null);
      setForm(formFromReading(r.reading, r.id, today(), m?.settings));
      return r;
    }, (r) => (r.duplicate ? "This receipt was uploaded before; using the saved copy" : "Receipt uploaded"));
    if (fileRef.current) fileRef.current.value = "";
  };
  const edit = (e: ExpenseView) => {
    setReading(null);
    setDuplicate(false);
    setEditing({ id: e.id, version: e.version });
    setForm(formFromExpense(e));
  };
  const save = () => {
    if (!form) return;
    const input = inputOfForm(form);
    void run(async () => {
      const r = editing ? await api.updateExpense(editing.id, { ...input, version: editing.version }) : await api.createExpense(input);
      resetForm();
      return r;
    }, editing ? "Expense updated" : "Expense saved");
  };
  const toggle = (id: string, on: boolean) => setPicked(on ? [...picked, id] : picked.filter((x) => x !== id));
  const note = form ? readingNote(reading, m?.ocrEngine ?? "") : null;
  const blockers = c ? submitBlockers(c, maxAge) : [];
  const unclaimed = m?.unclaimed ?? [];
  const addable = c?.status === "draft" ? picked.filter((id) => !c.lines.some((l) => l.id === id)) : [];

  return (
    <BentoTile span={12} title="My expenses" data-slice="3D" data-my-expenses
      action={
        <div className="flex gap-1">
          <input ref={fileRef} type="file" accept="image/*,application/pdf" capture="environment" hidden aria-label="Receipt photo or PDF" data-receipt-input
            onChange={(e) => onFile(e.target.files?.[0] ?? null)} />
          <Button size="sm" icon={<Camera strokeWidth={1.75} aria-hidden="true" />} iconPosition="start" data-action="capture-receipt" disabled={busy || !m} onClick={() => fileRef.current?.click()}>
            Snap a receipt
          </Button>
          <Button size="sm" variant="ghost" data-action="type-expense" disabled={busy || !m}
            onClick={() => {
              resetForm();
              setForm(blankForm(today()));
            }}>Type one in</Button>
        </div>
      }>
      {mine.isError && <EmptyState icon={Receipt} title="Expenses aren’t available" compact>Your staff record isn’t linked to this sign-in yet. Ask HR to link it.</EmptyState>}

      {form && note && (
        <div data-expense-form={editing ? "edit" : reading ? reading.status : "manual"}>
          <p data-reading-note={note.kind}>
            <Badge kind={note.kind}>{note.kind === "attention" ? "Low confidence" : reading?.status === "read" ? "Check" : "Note"}</Badge> {note.text}
            {duplicate && " This receipt had been uploaded before."}
          </p>
          <FormGrid columns={3}>
            <TextField label="Date" type="date" max={today()} value={form.spentOn} onChange={(e) => setForm({ ...form, spentOn: e.target.value })} />
            <TextField label="Merchant" value={form.merchant} placeholder="Shop, hotel or operator" onChange={(e) => setForm({ ...form, merchant: e.target.value })} />
            <TextField label={form.currency === "INR" ? "Amount (₹)" : `Amount (${form.currency})`} inputMode="decimal" value={form.amount} placeholder="378.00" onChange={(e) => setForm({ ...form, amount: e.target.value })}
              hint={form.currency !== "INR" ? "Only rupee expenses can be claimed." : undefined} />
            <FormSelect label="Category" value={form.categoryId} placeholder="Pick a category" options={categories} onChange={(e) => setForm({ ...form, categoryId: e.target.value })} />
            <FormSelect label="Paid by" value={form.paymentMethod} placeholder="—" options={PAYMENT_METHODS.map((p) => ({ value: p, label: p === "Mobile payment" ? "UPI / mobile payment" : p }))} onChange={(e) => setForm({ ...form, paymentMethod: e.target.value })} />
            <TextField label="GST paid (₹)" inputMode="decimal" value={form.tax} placeholder="CGST + SGST or IGST" onChange={(e) => setForm({ ...form, tax: e.target.value })} />
            <TextField label="Merchant GSTIN" value={form.gstin} placeholder="27AAAAA0000A1Z5" onChange={(e) => setForm({ ...form, gstin: e.target.value })} />
            <TextField label="Notes" value={form.notes} onChange={(e) => setForm({ ...form, notes: e.target.value })} />
            <KeyValue label="Receipt" value={<ReceiptThumb api={api} receiptId={form.receiptId} />} />
          </FormGrid>
          {form.items.length > 0 && <KeyValue label="Items on the receipt" value={form.items.map((i) => (i.amount === null ? i.name : `${i.name} ${inr(i.amount)}`)).join(" · ")} />}
          <div className="flex gap-2">
            <Button size="sm" data-action="save-expense" disabled={busy || !formReady(form, today())} onClick={save}>{editing ? "Save changes" : "Looks right, save"}</Button>
            <Button size="sm" variant="ghost" data-action="discard-expense" disabled={busy} onClick={resetForm}>Discard</Button>
          </div>
        </div>
      )}

      {m && (
        <>
          <FormGrid columns={2}>
            <KeyValue label="Not yet claimed" value={`${unclaimed.length} · ${inr(m.unclaimedTotal)}`} />
            <KeyValue label="Policy" value={`Receipt above ${inr(m.settings.receiptRequiredAbove)}${m.settings.maxAgeDays ? ` · claim within ${m.settings.maxAgeDays} days` : ""}`} />
          </FormGrid>
          <DataTable caption="Not yet claimed" rows={unclaimed} rowKey={(e) => e.id}
            empty={<EmptyState icon={Receipt} title="Nothing unclaimed" compact>Snap a receipt to add an expense.</EmptyState>}
            columns={[
              { key: "s", header: "", render: (e) => <Checkbox label={<span className="sr-only">Pick {e.merchant}</span>} checked={picked.includes(e.id)} onChange={(ev) => toggle(e.id, ev.target.checked)} data-pick={e.id} /> },
              { key: "r", header: "Receipt", render: (e) => <ReceiptThumb api={api} receiptId={e.receiptId} /> },
              { key: "d", header: "Date", render: (e) => formatDate(e.spentOn) },
              { key: "m", header: "Merchant", render: (e) => e.merchant },
              { key: "c", header: "Category", render: (e) => e.categoryName ?? "—" },
              { key: "a", header: "Amount", render: (e) => money(e.amount, e.currency) },
              { key: "f", header: "Checks", render: (e) => <FlagChips flags={e.flags} maxAgeDays={maxAge} /> },
              { key: "x", header: "", render: (e) => (
                <div className="flex gap-1">
                  <Button size="sm" variant="ghost" data-action="edit-expense" disabled={busy} onClick={() => edit(e)}>Edit</Button>
                  <Button size="sm" variant="ghost" data-action="delete-expense" disabled={busy} onClick={() => void run(async () => {
                    await api.deleteExpense(e.id);
                    setPicked(picked.filter((x) => x !== e.id));
                  }, "Expense deleted")}>Delete</Button>
                </div>
              ) },
            ]} />
          {picked.length > 0 && (
            <>
              <FormGrid columns={2}>
                <TextField label="Claim title" value={claimDraft.title} placeholder="Leave blank for “<Month> expenses”" onChange={(e) => setClaimDraft({ ...claimDraft, title: e.target.value })} />
                <TextField label="Purpose" value={claimDraft.purpose} placeholder="e.g. Paper evaluation at SPPU" onChange={(e) => setClaimDraft({ ...claimDraft, purpose: e.target.value })} />
              </FormGrid>
              <Button size="sm" data-action="create-claim" disabled={busy}
                onClick={() => void run(async () => {
                  const r = await api.createClaim({ title: claimDraft.title.trim() || null, purpose: claimDraft.purpose.trim() || null, expenseIds: picked });
                  setPicked([]);
                  setClaimDraft({ title: "", purpose: "" });
                  setOpenId(r.id);
                  return r;
                }, (r) => `${r.number} created as a draft`)}>
                Create claim from {picked.length} expense{picked.length === 1 ? "" : "s"}
              </Button>
            </>
          )}

          <DataTable caption="My claims" rows={m.claims} rowKey={(r) => r.id}
            empty={<EmptyState icon={Receipt} title="No claims yet" compact>Pick expenses above and create a claim.</EmptyState>}
            columns={[
              { key: "n", header: "Claim", render: (r) => r.number },
              { key: "t", header: "Title", render: (r) => r.title },
              { key: "a", header: "Total", render: (r) => inr(r.total) },
              { key: "s", header: "Status", render: (r) => (
                <span className="flex flex-wrap gap-1">
                  <Badge kind={statusKind(r.status)}>{STATUS_LABEL[r.status] ?? r.status}</Badge>
                  {r.returnedReason && <Badge kind="attention">Sent back</Badge>}
                </span>
              ) },
              { key: "o", header: "", render: (r) => <Button size="sm" variant={r.id === openId ? "primary" : "ghost"} data-action="open-my-claim" onClick={() => setOpenId(r.id === openId ? "" : r.id)}>{r.id === openId ? "Close" : "Open"}</Button> },
            ]} />
        </>
      )}

      {c && openId && (
        <div data-my-claim={c.id} data-status={c.status}>
          {c.returnedReason && (
            <div role="alert" data-returned-reason>
              <Badge kind="attention">Sent back by finance</Badge> <strong>{c.returnedReason}</strong>
              <p>Fix what they asked for, then submit again.</p>
            </div>
          )}
          <KeyValue label={`${c.number} · ${c.title}`} value={`${STATUS_LABEL[c.status] ?? c.status} · ${inr(c.total)} · ${c.withReceipt} of ${c.lineCount} with a receipt${c.paymentReference ? ` · paid, ref ${c.paymentReference}` : ""}`} />
          <DataTable caption="Lines" rows={c.lines} rowKey={(l) => l.id}
            empty={<EmptyState icon={Receipt} title="No lines" compact>Pick expenses above and add them.</EmptyState>}
            columns={[
              { key: "r", header: "Receipt", render: (l) => <ReceiptThumb api={api} receiptId={l.receiptId} /> },
              { key: "d", header: "Date", render: (l) => formatDate(l.spentOn) },
              { key: "m", header: "Merchant", render: (l) => l.merchant },
              { key: "a", header: "Amount", render: (l) => <OcrCompare line={l} /> },
              { key: "f", header: "Checks", render: (l) => <FlagChips flags={l.flags} maxAgeDays={maxAge} /> },
              { key: "x", header: "", render: (l) => (c.status !== "draft" ? null : (
                <div className="flex gap-1">
                  <Button size="sm" variant="ghost" data-action="edit-line" disabled={busy} onClick={() => edit(l)}>Edit</Button>
                  <Button size="sm" variant="ghost" data-action="remove-line" disabled={busy}
                    onClick={() => void run(() => api.updateClaim(c.id, { expenseIds: c.lines.filter((x) => x.id !== l.id).map((x) => x.id), version: c.version }), "Line removed")}>Remove</Button>
                </div>
              )) },
            ]} />
          {c.status === "draft" && (
            <>
              {addable.length > 0 && (
                <Button size="sm" variant="ghost" data-action="add-lines" disabled={busy}
                  onClick={() => void run(async () => {
                    const r = await api.updateClaim(c.id, { expenseIds: [...c.lines.map((l) => l.id), ...addable], version: c.version });
                    setPicked([]);
                    return r;
                  }, "Added to the claim")}>Add {addable.length} picked expense{addable.length === 1 ? "" : "s"} to this claim</Button>
              )}
              {blockers.length > 0 && (
                <ul data-submit-blockers>
                  {blockers.map((b) => <li key={b}>{b}</li>)}
                </ul>
              )}
              <div className="flex gap-2">
                <Button size="sm" data-action="submit-claim" disabled={busy || !c.canSubmit} title={blockers.join(" ") || undefined}
                  onClick={() => void run(() => api.submit(c.id, c.version), `${c.number} submitted for approval`)}>Submit claim</Button>
                <Button size="sm" variant="ghost" data-action="delete-claim" disabled={busy}
                  onClick={() => void run(async () => {
                    await api.deleteClaim(c.id, c.version);
                    setOpenId("");
                  }, "Claim deleted; its expenses are unclaimed again")}>Delete claim</Button>
              </div>
            </>
          )}
          {c.status === "submitted" && (
            <Button size="sm" variant="ghost" data-action="withdraw-claim" disabled={busy} onClick={() => void run(() => api.withdraw(c.id, c.version), `${c.number} withdrawn to draft`)}>Withdraw</Button>
          )}
          {c.flags.length > 0 && c.status !== "draft" && <p>{c.flags.map((f) => flagLabel(f, maxAge)).join(" · ")}</p>}
        </div>
      )}
      {dialog}
    </BentoTile>
  );
}

