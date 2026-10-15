/**
 * Slice **8D** web components (procurement, inventory and assets). Export names are imported by
 * the foundation frames (docs/batch8-contracts.md §10); every screen carries `data-slice="8D"`
 * and uses @sutra/ui only. Money and quantities are shown exactly as the API returns them
 * (decimal strings).
 *
 * - `ProcurementAdminTab` — Admin → Campus → Procurement: suppliers (registration → approval →
 *   suspension), requisitions, sourcing events (quotations → evaluation → award), purchase orders
 *   (issue → acknowledge → receive → close), goods receipts, vendor invoices (three-way match →
 *   post) and payments.
 * - `InventoryAdminTab` — items, stores, stock on hand, issues and returns, reservations,
 *   adjustments, reorder-level run.
 * - `AssetsAdminTab` — asset register, custody, condition and retirement.
 */
import { Boxes, ClipboardList, FileText, Package, PackageCheck, Receipt, ShieldAlert, Truck, Wrench } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, formatNumber, KpiTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { fmtDate, NoInstitution, REASON, todayIso, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { Actions, CodeName, inr } from "../ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { useWorkforceApi, wkeys } from "../../people/workforce/hooks";
import { fkeys, useFacilitiesApi } from "../facilities/hooks";
import { rkeys, useResourcesApi } from "./hooks";
import type { Asset, GoodsReceipt, PurchaseOrder, Requisition, SourcingEvent, Supplier, VendorInvoice } from "./types";
import { formatInr } from "../../finance";

export { ResourcesApiProvider } from "./hooks";

const dash = (v: string | null | undefined) => (v === null || v === undefined || v === "" ? "—" : v);

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="8D">
      <NoInstitution />
    </BentoTile>
  );
}

// ================================================================================= action rules

/** Next steps offered for a supplier. */
export function supplierActions(s: Pick<Supplier, "status" | "approvalPending">): Array<"register" | "submit" | "approve" | "reject" | "suspend" | "restore"> {
  if (s.approvalPending) return ["approve", "reject"];
  switch (s.status) {
    case "prospective":
      return ["register"];
    case "registered":
      return ["submit"];
    case "active":
      return ["suspend"];
    case "suspended":
      return ["restore"];
    default:
      return [];
  }
}

/** Next steps offered for a requisition. */
export function requisitionActions(r: Pick<Requisition, "status">): Array<"submit" | "approve" | "reject" | "source" | "order" | "cancel"> {
  switch (r.status) {
    case "draft":
    case "amended":
      return ["submit", "cancel"];
    case "submitted":
      return ["approve", "reject"];
    case "released":
      return ["source", "order", "cancel"];
    case "approved":
    case "rejected":
    case "withdrawn":
      return ["cancel"];
    default:
      return [];
  }
}

/** Next steps offered for a purchase order. */
export function orderActions(o: Pick<PurchaseOrder, "status" | "approvalPending">): Array<"issue" | "approve" | "reject" | "acknowledge" | "receive" | "invoice" | "close" | "cancel"> {
  if (o.approvalPending) return ["approve", "reject"];
  switch (o.status) {
    case "draft":
      return ["issue", "cancel"];
    case "issued":
      return ["acknowledge", "receive", "cancel"];
    case "acknowledged":
      return ["receive", "cancel"];
    case "partially_received":
      return ["receive", "invoice"];
    case "received":
      return ["invoice", "close"];
    default:
      return [];
  }
}

/** Next steps offered for a vendor invoice (a mismatch never posts). */
export function invoiceActions(i: Pick<VendorInvoice, "status" | "approvalPending" | "isCreditNote">): Array<"match" | "approve" | "reject" | "post" | "pay"> {
  if (i.approvalPending) return ["approve", "reject"];
  switch (i.status) {
    case "received":
    case "mismatch":
      return ["match"];
    case "approved":
      return ["post"];
    case "posted":
      return i.isCreditNote ? [] : ["pay"];
    default:
      return [];
  }
}

/** Next steps offered for an asset. */
export function assetActions(a: Pick<Asset, "status" | "retirementPending">): Array<"assign" | "return" | "condition" | "retire" | "approve" | "reject"> {
  if (a.retirementPending) return ["approve", "reject"];
  switch (a.status) {
    case "registered":
    case "available":
      return ["assign", "condition", "retire"];
    case "assigned":
      return ["return", "condition"];
    case "under_repair":
      return ["condition", "retire"];
    default:
      return [];
  }
}

/** `received − invoiced` of an order line, as a decimal string (thousandths, no floats). */
export function toInvoice(received: string, invoiced: string): string {
  const m = (v: string) => {
    const [i = "0", f = ""] = v.replace("-", "").split(".");
    const n = Number(i) * 1000 + Number((f + "000").slice(0, 3));
    return v.startsWith("-") ? -n : n;
  };
  const d = Math.max(0, m(received) - m(invoiced));
  return `${Math.floor(d / 1000)}.${String(d % 1000).padStart(3, "0")}`;
}

// ================================================================================= procurement

/** Admin → Campus → Procurement. */
export function ProcurementAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <Procurement institution={institution} />;
}

function Procurement({ institution }: { institution: Institution }) {
  const api = useResourcesApi();
  const inst = institution.id;
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const suppliers = useQuery({ queryKey: rkeys.scoped(inst, "suppliers"), queryFn: () => api.suppliers(inst) });
  const reqs = useQuery({ queryKey: rkeys.scoped(inst, "requisitions"), queryFn: () => api.requisitions(inst) });
  const events = useQuery({ queryKey: rkeys.scoped(inst, "sourcing"), queryFn: () => api.sourcingEvents(inst) });
  const orders = useQuery({ queryKey: rkeys.scoped(inst, "orders"), queryFn: () => api.purchaseOrders(inst) });
  const receipts = useQuery({ queryKey: rkeys.scoped(inst, "receipts"), queryFn: () => api.receipts(inst) });
  const invoices = useQuery({ queryKey: rkeys.scoped(inst, "invoices"), queryFn: () => api.invoices(inst) });
  const payments = useQuery({ queryKey: rkeys.scoped(inst, "payments"), queryFn: () => api.payments(inst) });
  const items = useQuery({ queryKey: rkeys.scoped(inst, "items"), queryFn: () => api.items(inst) });
  const stores = useQuery({ queryKey: rkeys.scoped(inst, "stores"), queryFn: () => api.stores(inst) });
  const activeSuppliers = (suppliers.data ?? []).filter((s) => s.status === "active");
  const supplierOptions = activeSuppliers.map((s) => ({ value: s.id, label: `${s.number} · ${s.legalName}` }));
  const count = <T,>(list: T[] | undefined, f: (x: T) => boolean) => (list ? String(list.filter(f).length) : "…");

  // ---- suppliers
  const newSupplier = () =>
    ask({
      title: "New supplier",
      name: "supplier-create",
      submitLabel: "Create",
      fields: [
        { name: "legalName", label: "Legal name", required: true, wide: true },
        { name: "gstin", label: "GSTIN", placeholder: "27ABCDE1234F1Z5" },
        { name: "pan", label: "PAN", placeholder: "ABCDE1234F" },
        { name: "email", label: "E-mail" },
        { name: "bankAccountNumber", label: "Bank account no." },
        { name: "bankIfsc", label: "IFSC", placeholder: "HDFC0001234" },
      ],
      onSubmit: (v) =>
        submit(
          () =>
            api.createSupplier(inst, {
              legalName: v.legalName ?? "",
              gstin: v.gstin || undefined,
              pan: v.pan || undefined,
              email: v.email || undefined,
              bankAccountName: v.bankAccountNumber ? v.legalName : undefined,
              bankAccountNumber: v.bankAccountNumber || undefined,
              bankIfsc: v.bankIfsc || undefined,
            }),
          (s) => `${s.number} created (prospective)`,
        ),
    });
  const onSupplier = (s: Supplier, a: ReturnType<typeof supplierActions>[number]) => {
    if (a === "approve") return run(() => api.decideSupplier(s.id, s.version, "approve"), `${s.number} approved`);
    if (a === "reject") return ask({ title: `Reject · ${s.number}`, name: "supplier-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideSupplier(s.id, s.version, "reject", v.reason), `${s.number} sent back`) });
    if (a === "suspend" || a === "restore")
      return ask({ title: `${statusLabel(a)} · ${s.number}`, name: `supplier-${a}`, submitLabel: statusLabel(a), fields: [REASON], onSubmit: (v) => submit(() => api.supplierCommand(s.id, s.version, a, v.reason), `${s.number}: ${statusLabel(a)}`) });
    return run(() => api.supplierCommand(s.id, s.version, a), `${s.number}: ${statusLabel(a)}`);
  };

  // ---- requisitions
  const itemOptions = [{ value: "", label: "Free text (service / non-catalogue)" }, ...(items.data ?? []).filter((i) => i.status === "active").map((i) => ({ value: i.id, label: `${i.code} · ${i.name}` }))];
  const newRequisition = () =>
    ask({
      title: "New purchase requisition",
      name: "requisition-create",
      submitLabel: "Create",
      fields: [
        { name: "title", label: "Title", required: true, wide: true },
        { name: "itemId", label: "Item", type: "select", options: itemOptions, initial: "" },
        { name: "description", label: "Description (free text)" },
        { name: "quantity", label: "Quantity", required: true, initial: "1" },
        { name: "estimatedUnitCost", label: "Estimated unit cost (₹)", placeholder: "Item standard cost" },
      ],
      onSubmit: (v) =>
        submit(
          () => api.createRequisition(inst, { title: v.title ?? "", lines: [{ itemId: v.itemId || undefined, description: v.description || undefined, quantity: v.quantity ?? "1", estimatedUnitCost: v.estimatedUnitCost || undefined }] }),
          (r) => `${r.number} drafted · ${formatInr(r.estimatedTotal)}`,
        ),
    });
  const onRequisition = (r: Requisition, a: ReturnType<typeof requisitionActions>[number]) => {
    if (a === "submit") return run(() => api.submitRequisition(r.id, r.version), (x) => `${x.number}: ${statusLabel(x.status)}`);
    if (a === "approve") return run(() => api.decideRequisition(r.id, r.version, "approve"), `${r.number} approved and released`);
    if (a === "reject") return ask({ title: `Reject · ${r.number}`, name: "requisition-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideRequisition(r.id, r.version, "reject", v.reason), `${r.number} rejected`) });
    if (a === "cancel") return ask({ title: `Cancel · ${r.number}`, name: "requisition-cancel", submitLabel: "Cancel requisition", fields: [REASON], onSubmit: (v) => submit(() => api.cancelRequisition(r.id, r.version, v.reason ?? ""), `${r.number} cancelled`) });
    if (a === "source")
      return ask({
        title: `Source · ${r.number}`,
        name: "sourcing-create",
        submitLabel: "Open",
        fields: [
          {
            name: "method",
            label: "Method",
            type: "select",
            options: [
              { value: "rfq", label: "Request for quotation" },
              { value: "direct", label: "Direct (small value, one quotation)" },
            ],
            initial: "rfq",
          },
        ],
        onSubmit: (v) => submit(() => api.createSourcingEvent(inst, { requisitionId: r.id, method: v.method === "direct" ? "direct" : "rfq" }), (e) => `${e.number} open for quotations`),
      });
    return ask({
      title: `Direct order · ${r.number}`,
      name: "order-direct",
      submitLabel: "Draft order",
      fields: [{ name: "supplierId", label: "Supplier", type: "select", options: supplierOptions, initial: supplierOptions[0]?.value ?? "", required: true }],
      onSubmit: (v) => submit(() => api.createPurchaseOrder(inst, { requisitionId: r.id, supplierId: v.supplierId, deliveryLocationId: stores.data?.[0]?.id }), (o) => `${o.number} drafted · ${formatInr(o.totalAmount)}`),
    });
  };

  // ---- sourcing
  const quote = async (e: SourcingEvent) => {
    const d = await api.sourcingEvent(e.id);
    const priceFields: FieldSpec[] = (d.lines ?? []).map((l) => ({ name: `price-${l.lineNo}`, label: `${l.description} × ${l.quantity} — unit price (₹)`, required: true, initial: l.estimatedUnitCost, wide: true }));
    ask({
      title: `Quotation · ${d.number}`,
      name: "sourcing-quote",
      submitLabel: "Record quotation",
      fields: [{ name: "supplierId", label: "Supplier", type: "select", options: supplierOptions, initial: supplierOptions[0]?.value ?? "", required: true, wide: true }, ...priceFields],
      onSubmit: (v) => submit(() => api.quote(d.id, d.version, { supplierId: v.supplierId ?? "", lines: (d.lines ?? []).map((l) => ({ lineNo: l.lineNo, unitPrice: v[`price-${l.lineNo}`] ?? "" })) }), `${d.number}: quotation recorded`),
    });
  };
  const award = async (e: SourcingEvent) => {
    const d = await api.sourcingEvent(e.id);
    const qs = d.quotations ?? [];
    ask({
      title: `Award · ${d.number}`,
      name: "sourcing-award",
      submitLabel: "Award",
      fields: [
        { name: "quotationId", label: "Quotation", type: "select", options: qs.map((q) => ({ value: q.id, label: `${q.rank ? `#${q.rank} ` : ""}${q.supplierNumber} · ${formatInr(q.totalAmount)}` })), initial: qs[0]?.id ?? "", required: true, wide: true },
        { name: "reason", label: "Justification (required unless lowest)", wide: true },
      ],
      onSubmit: (v) => submit(() => api.award(d.id, d.version, v.quotationId ?? "", v.reason || undefined), (x) => `${x.number}: ${statusLabel(x.status)}`),
    });
  };
  const sourcingRow = (e: SourcingEvent): RowAction[] => {
    const acts: RowAction[] = [];
    if (e.status === "open") acts.push({ id: "sourcing-quote", label: "Quotation", onClick: () => void quote(e) });
    if (e.status === "open" && e.method === "rfq") acts.push({ id: "sourcing-evaluate", label: "Evaluate", onClick: () => void run(() => api.evaluate(e.id, e.version), `${e.number}: evaluating`) });
    if ((e.status === "evaluating" || (e.status === "open" && e.method === "direct")) && !e.pendingQuotationId) acts.push({ id: "sourcing-award", label: "Award", variant: "primary", onClick: () => void award(e) });
    // One order per award: once it exists (and is not cancelled) the award is used up.
    if (e.status === "awarded" && !(orders.data ?? []).some((o) => o.sourcingEventId === e.id && o.status !== "cancelled"))
      acts.push({ id: "order-from-award", label: "Create order", variant: "primary", onClick: () => void run(() => api.createPurchaseOrder(inst, { sourcingEventId: e.id, deliveryLocationId: stores.data?.[0]?.id }), (o) => `${o.number} drafted · ${formatInr(o.totalAmount)}`) });
    return acts;
  };

  // ---- purchase orders
  const receive = async (o: PurchaseOrder) => {
    const d = await api.purchaseOrder(o.id);
    const open = (d.lines ?? []).filter((l) => l.lineKind !== "service" || l.receivedQuantity !== l.quantity);
    ask({
      title: `Goods receipt · ${d.number}`,
      name: "receipt-create",
      submitLabel: "Record delivery",
      fields: open.map((l) => ({ name: `qty-${l.lineNo}`, label: `${l.description} (ordered ${l.quantity}, received ${l.receivedQuantity})`, initial: toInvoice(l.quantity, l.receivedQuantity), wide: true })),
      onSubmit: (v) =>
        submit(
          () =>
            api.receive(d.id, {
              lines: open.map((l) => ({ lineNo: l.lineNo, quantity: v[`qty-${l.lineNo}`] ?? "0" })).filter((l) => l.quantity.trim() !== "" && Number(l.quantity) > 0),
            }),
          (g) => `${g.number} received — inspect and accept it below`,
        ),
    });
  };
  const invoice = async (o: PurchaseOrder) => {
    const d = await api.purchaseOrder(o.id);
    const lines = (d.lines ?? []).map((l) => ({ lineNo: l.lineNo, quantity: toInvoice(l.receivedQuantity, l.invoicedQuantity), unitPrice: l.unitPrice })).filter((l) => l.quantity !== "0.000");
    ask({
      title: `Vendor invoice · ${d.number}`,
      name: "invoice-create",
      submitLabel: "Record invoice",
      fields: [
        { name: "supplierInvoiceNumber", label: "Supplier invoice no.", required: true },
        { name: "invoiceDate", label: "Invoice date", type: "date", required: true, initial: todayIso() },
      ],
      onSubmit: (v) => submit(() => api.createInvoice(inst, { purchaseOrderId: d.id, supplierInvoiceNumber: v.supplierInvoiceNumber ?? "", invoiceDate: v.invoiceDate ?? todayIso(), lines }), (x) => `${x.number} recorded · ${formatInr(x.totalAmount)} — run the three-way match`),
    });
  };
  const onOrder = (o: PurchaseOrder, a: ReturnType<typeof orderActions>[number]) => {
    switch (a) {
      case "issue":
        return run(() => api.issueOrder(o.id, o.version), (x) => (x.approvalPending ? `${x.number} awaits approval` : `${x.number} issued to ${x.supplierNumber}`));
      case "approve":
        return run(() => api.decideOrder(o.id, o.version, "approve"), `${o.number} approved and issued`);
      case "reject":
        return ask({ title: `Reject · ${o.number}`, name: "order-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideOrder(o.id, o.version, "reject", v.reason), `${o.number} sent back`) });
      case "acknowledge":
        return ask({ title: `Acknowledgement · ${o.number}`, name: "order-acknowledge", submitLabel: "Record", fields: [{ name: "reference", label: "Supplier reference", required: true }], onSubmit: (v) => submit(() => api.acknowledgeOrder(o.id, o.version, v.reference ?? ""), `${o.number} acknowledged`) });
      case "receive":
        return void receive(o);
      case "invoice":
        return void invoice(o);
      case "close":
        return run(() => api.closeOrder(o.id, o.version), `${o.number} closed`);
      case "cancel":
        return ask({ title: `Cancel · ${o.number}`, name: "order-cancel", submitLabel: "Cancel order", fields: [REASON], onSubmit: (v) => submit(() => api.cancelOrder(o.id, o.version, v.reason ?? ""), `${o.number} cancelled`) });
    }
  };

  // ---- receipts, invoices
  const onReceipt = (g: GoodsReceipt, a: "accept" | "reject") =>
    a === "accept"
      ? run(() => api.acceptReceipt(g.id, g.version), (x) => `${x.number}: ${statusLabel(x.status)}`)
      : ask({ title: `Reject · ${g.number}`, name: "receipt-reject", submitLabel: "Reject delivery", fields: [REASON], onSubmit: (v) => submit(() => api.rejectReceipt(g.id, g.version, v.reason ?? ""), `${g.number} rejected`) });
  const onInvoice = (i: VendorInvoice, a: ReturnType<typeof invoiceActions>[number]) => {
    switch (a) {
      case "match":
        return run(() => api.matchInvoice(i.id, i.version), (x) => (x.status === "mismatch" ? `${x.number}: mismatch — ${x.matchFindings.map((f) => f.message).join("; ")}` : `${x.number}: ${statusLabel(x.status)}`));
      case "approve":
        return run(() => api.decideInvoice(i.id, i.version, "approve"), `${i.number} approved`);
      case "reject":
        return ask({ title: `Reject · ${i.number}`, name: "invoice-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideInvoice(i.id, i.version, "reject", v.reason), `${i.number} rejected`) });
      case "post":
        return run(() => api.postInvoice(i.id, i.version), (x) => `${x.number} posted (${dash(x.entryNumber)})`);
      case "pay":
        return ask({
          title: `Pay · ${i.number}`,
          name: "payment-create",
          submitLabel: "Pay and post",
          fields: [
            { name: "amount", label: "Amount (₹)", required: true, initial: toInvoice(i.totalAmount, i.paidAmount).replace(/0$/, "") },
            { name: "paymentDate", label: "Payment date", type: "date", required: true, initial: todayIso() },
            { name: "reference", label: "Bank reference" },
          ],
          onSubmit: (v) =>
            submit(async () => {
              const p = await api.createPayment(inst, { supplierId: i.supplierId, paymentDate: v.paymentDate ?? todayIso(), reference: v.reference || undefined, allocations: [{ vendorInvoiceId: i.id, amount: v.amount ?? "" }] });
              return api.postPayment(p.id, p.version);
            }, (p) => `${p.number} posted (${dash(p.entryNumber)})`),
        });
    }
  };

  return (
    <>
      <KpiTile icon={Truck} tone="orange" label="Active suppliers" value={count(suppliers.data, (s) => s.status === "active")} sub="Approved and orderable" data-kpi="proc-suppliers" />
      <KpiTile icon={ClipboardList} tone="violet" label="Requisitions to decide" value={count(reqs.data, (r) => r.status === "submitted")} sub="Awaiting approval" data-kpi="proc-requisitions" />
      <KpiTile icon={Package} tone="blue" label="Open purchase orders" value={count(orders.data, (o) => ["issued", "acknowledged", "partially_received"].includes(o.status))} sub="Issued, not fully received" data-kpi="proc-orders" />
      <KpiTile icon={ShieldAlert} tone="rose" label="Invoice mismatches" value={count(invoices.data, (i) => i.status === "mismatch")} sub="Blocked from posting" data-kpi="proc-mismatch" />
      <BentoTile
        span={12}
        title="Suppliers"
        data-slice="8D"
        data-suppliers
        action={
          <Button size="sm" variant="ghost" data-action="supplier-new" disabled={busy} onClick={newSupplier}>
            New supplier
          </Button>
        }
      >
        <DataTable
          caption="Suppliers"
          rows={suppliers.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={Truck} title="No suppliers" compact>Register the first supplier to start sourcing.</EmptyState>}
          columns={[
            { key: "name", header: "Supplier", render: (s) => <CodeName name={s.legalName} code={s.number} /> },
            { key: "gstin", header: "GSTIN", render: (s) => dash(s.gstin), muted: true },
            { key: "bank", header: "Bank account", render: (s) => dash(s.bankAccountNumber), muted: true },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.approvalPending ? "pending_approval" : s.status} /> },
            {
              key: "actions",
              header: "",
              render: (s) => <Actions disabled={busy} actions={supplierActions(s).map((a): RowAction => ({ id: `supplier-${a}`, label: statusLabel(a), variant: a === "approve" || a === "submit" ? "primary" : "ghost", onClick: () => void onSupplier(s, a) }))} />,
            },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Purchase requisitions"
        data-slice="8D"
        data-requisitions
        action={
          <Button size="sm" variant="ghost" data-action="requisition-new" disabled={busy} onClick={newRequisition}>
            New requisition
          </Button>
        }
      >
        <DataTable
          caption="Requisitions"
          rows={reqs.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={ClipboardList} title="No requisitions" compact>Raise demand for goods or services.</EmptyState>}
          columns={[
            { key: "title", header: "Requisition", render: (r) => <CodeName name={r.title} code={`${r.number}${r.revision > 1 ? ` · rev ${r.revision}` : ""}`} /> },
            { key: "total", header: "Estimate", render: (r) => inr(r.estimatedTotal) },
            { key: "source", header: "Source", render: (r) => statusLabel(r.source), muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            {
              key: "actions",
              header: "",
              render: (r) => <Actions disabled={busy} actions={requisitionActions(r).map((a): RowAction => ({ id: `requisition-${a}`, label: statusLabel(a), variant: a === "submit" || a === "approve" ? "primary" : "ghost", onClick: () => void onRequisition(r, a) }))} />,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Sourcing events" data-slice="8D" data-sourcing>
        <DataTable
          caption="Sourcing events"
          rows={events.data ?? []}
          rowKey={(e) => e.id}
          empty={<EmptyState icon={FileText} title="No sourcing events" compact>Source a released requisition by RFQ or directly.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (e) => e.number },
            { key: "req", header: "Requisition", render: (e) => e.requisitionNumber, muted: true },
            { key: "method", header: "Method", render: (e) => (e.method === "rfq" ? "RFQ" : "Direct") },
            { key: "value", header: "Estimate (₹)", render: (e) => e.estimatedValue },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.pendingQuotationId ? "pending_approval" : e.status} /> },
            { key: "actions", header: "", render: (e) => <Actions disabled={busy} actions={sourcingRow(e)} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Purchase orders" data-slice="8D" data-orders>
        <DataTable
          caption="Purchase orders"
          rows={orders.data ?? []}
          rowKey={(o) => o.id}
          empty={<EmptyState icon={Package} title="No purchase orders" compact>Create an order from an award or a released requisition.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (o) => `${o.number}${o.revision > 1 ? ` · rev ${o.revision}` : ""}` },
            { key: "supplier", header: "Supplier", render: (o) => <CodeName name={o.supplierName} code={o.supplierNumber} /> },
            { key: "date", header: "Ordered", render: (o) => fmtDate(o.orderDate), muted: true },
            { key: "total", header: "Total", render: (o) => inr(o.totalAmount) },
            { key: "status", header: "Status", render: (o) => <StatusBadge status={o.approvalPending ? "pending_approval" : o.status} /> },
            {
              key: "actions",
              header: "",
              render: (o) => <Actions disabled={busy} actions={orderActions(o).map((a): RowAction => ({ id: `order-${a}`, label: statusLabel(a), variant: a === "issue" || a === "approve" || a === "receive" ? "primary" : "ghost", onClick: () => void onOrder(o, a) }))} />,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Goods receipts" data-slice="8D" data-receipts>
        <DataTable
          caption="Goods receipts"
          rows={receipts.data ?? []}
          rowKey={(g) => g.id}
          empty={<EmptyState icon={PackageCheck} title="No deliveries" compact>Record a delivery against an issued order.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (g) => g.number },
            { key: "po", header: "Order", render: (g) => g.purchaseOrderNumber, muted: true },
            { key: "date", header: "Received", render: (g) => fmtDate(g.receivedOn) },
            { key: "status", header: "Status", render: (g) => <StatusBadge status={g.status} /> },
            {
              key: "actions",
              header: "",
              render: (g) =>
                g.status === "received" || g.status === "inspecting" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      { id: "receipt-accept", label: "Accept", variant: "primary", onClick: () => void onReceipt(g, "accept") },
                      { id: "receipt-reject", label: "Reject", onClick: () => void onReceipt(g, "reject") },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Vendor payments" data-slice="8D" data-payments>
        <DataTable
          caption="Vendor payments"
          rows={payments.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={Receipt} title="No payments" compact>Pay posted invoices from the invoice list.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (p) => p.number },
            { key: "supplier", header: "Supplier", render: (p) => p.supplierNumber, muted: true },
            { key: "amount", header: "Amount", render: (p) => inr(p.amount) },
            { key: "entry", header: "Journal", render: (p) => dash(p.entryNumber), muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Vendor invoices" data-slice="8D" data-invoices>
        <DataTable
          caption="Vendor invoices"
          rows={invoices.data ?? []}
          rowKey={(i) => i.id}
          empty={<EmptyState icon={Receipt} title="No vendor invoices" compact>Record an invoice against a received order.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (i) => `${i.number}${i.isCreditNote ? " · credit note" : ""}` },
            { key: "ref", header: "Supplier invoice", render: (i) => `${i.supplierNumber} · ${i.supplierInvoiceNumber}`, muted: true },
            { key: "po", header: "Order", render: (i) => i.purchaseOrderNumber, muted: true },
            { key: "total", header: "Total", render: (i) => inr(i.totalAmount) },
            { key: "paid", header: "Paid", render: (i) => inr(i.paidAmount) },
            { key: "match", header: "Three-way match", render: (i) => (i.matchFindings.length ? i.matchFindings.map((f) => `L${f.lineNo}: ${f.message}`).join("; ") : i.status === "received" ? "Not matched yet" : "Matched") },
            { key: "status", header: "Status", render: (i) => <StatusBadge status={i.approvalPending ? "pending_approval" : i.status} /> },
            {
              key: "actions",
              header: "",
              render: (i) => <Actions disabled={busy} actions={invoiceActions(i).map((a): RowAction => ({ id: `invoice-${a}`, label: statusLabel(a), variant: a === "post" || a === "match" ? "primary" : "ghost", onClick: () => void onInvoice(i, a) }))} />,
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================================= inventory

/** Admin → Campus → Inventory. */
export function InventoryAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <Inventory institution={institution} />;
}

function Inventory({ institution }: { institution: Institution }) {
  const api = useResourcesApi();
  const inst = institution.id;
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const items = useQuery({ queryKey: rkeys.scoped(inst, "items"), queryFn: () => api.items(inst) });
  const stores = useQuery({ queryKey: rkeys.scoped(inst, "stores"), queryFn: () => api.stores(inst) });
  const stock = useQuery({ queryKey: rkeys.scoped(inst, "stock"), queryFn: () => api.stock(inst) });
  const issues = useQuery({ queryKey: rkeys.scoped(inst, "issues"), queryFn: () => api.issues(inst) });
  const reservations = useQuery({ queryKey: rkeys.scoped(inst, "reservations"), queryFn: () => api.reservations(inst) });
  const adjustments = useQuery({ queryKey: rkeys.scoped(inst, "adjustments"), queryFn: () => api.adjustments(inst) });
  const needs = useQuery({ queryKey: rkeys.scoped(inst, "replenishment"), queryFn: () => api.replenishment(inst) });
  const activeItems = (items.data ?? []).filter((i) => i.status === "active" && i.kind !== "service").map((i) => ({ value: i.id, label: `${i.code} · ${i.name}` }));
  const locations = (stores.data ?? []).filter((s) => s.status === "active").map((s) => ({ value: s.id, label: `${s.code} · ${s.name}` }));
  const stockFields = (extra: FieldSpec[]): FieldSpec[] => [
    { name: "itemId", label: "Item", type: "select", options: activeItems, initial: activeItems[0]?.value ?? "", required: true, wide: true },
    { name: "locationId", label: "Store / location", type: "select", options: locations, initial: locations[0]?.value ?? "", required: true },
    ...extra,
  ];
  const count = <T,>(list: T[] | undefined, f: (x: T) => boolean) => (list ? String(list.filter(f).length) : "…");

  const newItem = () =>
    ask({
      title: "New item",
      name: "item-create",
      submitLabel: "Create",
      fields: [
        { name: "code", label: "Code", required: true, placeholder: "PAPER-A4" },
        { name: "name", label: "Name", required: true },
        {
          name: "kind",
          label: "Kind",
          type: "select",
          options: [
            { value: "consumable", label: "Consumable (stocked)" },
            { value: "asset", label: "Asset (registered)" },
            { value: "service", label: "Service" },
          ],
          initial: "consumable",
        },
        { name: "unit", label: "Unit", placeholder: "each" },
        { name: "reorderLevel", label: "Reorder level" },
        { name: "standardCost", label: "Standard cost (₹)" },
      ],
      onSubmit: (v) =>
        submit(
          () => api.createItem(inst, { code: v.code ?? "", name: v.name ?? "", kind: v.kind ?? "consumable", unit: v.unit || undefined, reorderLevel: v.reorderLevel || undefined, standardCost: v.standardCost || undefined }),
          (i) => `${i.code} created (draft) — activate it to stock and buy it`,
        ),
    });
  const newStore = () =>
    ask({
      title: "New store",
      name: "store-create",
      submitLabel: "Create",
      fields: [
        { name: "code", label: "Code", required: true, placeholder: "CENTRAL" },
        { name: "name", label: "Name", required: true },
      ],
      onSubmit: (v) => submit(() => api.createStore(inst, { code: v.code ?? "", name: v.name ?? "" }), (s) => `Store ${s.code} created`),
    });
  const newIssue = () =>
    ask({
      title: "Stock issue",
      name: "issue-create",
      submitLabel: "Request",
      fields: stockFields([
        { name: "quantity", label: "Quantity", required: true },
        { name: "purpose", label: "Purpose", required: true, wide: true },
      ]),
      onSubmit: (v) => submit(() => api.createIssue(inst, { itemId: v.itemId ?? "", locationId: v.locationId ?? "", quantity: v.quantity ?? "", purpose: v.purpose ?? "" }), (s) => `${s.number} requested`),
    });
  const newReservation = () =>
    ask({
      title: "Reserve stock",
      name: "reservation-create",
      submitLabel: "Reserve",
      fields: stockFields([
        { name: "quantity", label: "Quantity", required: true },
        { name: "purpose", label: "Purpose", required: true, wide: true },
      ]),
      onSubmit: (v) => submit(() => api.reserve(inst, { itemId: v.itemId ?? "", locationId: v.locationId ?? "", quantity: v.quantity ?? "", purpose: v.purpose ?? "" }), (r) => `${r.quantity} of ${r.itemCode} reserved`),
    });
  const newAdjustment = () =>
    ask({
      title: "Stock adjustment",
      name: "adjustment-create",
      submitLabel: "Request",
      fields: stockFields([
        { name: "quantity", label: "Quantity (− lost, + found)", required: true, placeholder: "-2" },
        { name: "reason", label: "Reason", required: true, wide: true },
      ]),
      onSubmit: (v) => submit(() => api.adjust(inst, { itemId: v.itemId ?? "", locationId: v.locationId ?? "", quantity: v.quantity ?? "", reason: v.reason ?? "" }), (a) => `Adjustment ${a.quantity} ${a.itemCode}: ${statusLabel(a.status)}`),
    });
  const runReplenishment = () =>
    ask({
      title: "Reorder-level check",
      name: "replenishment-run",
      submitLabel: "Run",
      fields: [{ name: "draftRequisition", label: "Also draft a purchase requisition", type: "checkbox", initial: "true" }],
      onSubmit: (v) => submit(() => api.runReplenishment(inst, v.draftRequisition === "true"), (r) => `${r.processed} replenishment need(s) raised, ${r.skipped} item(s) above reorder level`),
    });

  return (
    <>
      <KpiTile icon={Boxes} tone="orange" label="Active items" value={count(items.data, (i) => i.status === "active")} sub="Item master" data-kpi="inv-items" />
      <KpiTile icon={ShieldAlert} tone="amber" label="At reorder level" value={count(stock.data, (l) => l.atReorderLevel)} sub="Stock lines to replenish" data-kpi="inv-reorder" />
      <KpiTile icon={ClipboardList} tone="violet" label="Issues to hand over" value={count(issues.data, (s) => s.status === "requested" || s.status === "approved")} sub="Requested or approved" data-kpi="inv-issues" />
      <KpiTile icon={Package} tone="blue" label="Active reservations" value={count(reservations.data, (r) => r.status === "active")} sub="Held stock" data-kpi="inv-reservations" />
      <BentoTile
        span={8}
        title="Items"
        data-slice="8D"
        data-items
        action={
          <Button size="sm" variant="ghost" data-action="item-new" disabled={busy} onClick={newItem}>
            New item
          </Button>
        }
      >
        <DataTable
          caption="Items"
          rows={items.data ?? []}
          rowKey={(i) => i.id}
          empty={<EmptyState icon={Boxes} title="No items" compact>Create the item master first.</EmptyState>}
          columns={[
            { key: "name", header: "Item", render: (i) => <CodeName name={i.name} code={i.code} /> },
            { key: "kind", header: "Kind", render: (i) => statusLabel(i.kind), muted: true },
            { key: "onHand", header: "On hand", render: (i) => `${formatNumber(i.onHand)} ${i.unit}` },
            { key: "reorder", header: "Reorder at", render: (i) => formatNumber(i.reorderLevel), muted: true },
            { key: "status", header: "Status", render: (i) => <StatusBadge status={i.status} /> },
            {
              key: "actions",
              header: "",
              render: (i) =>
                i.status === "draft" || i.status === "inactive" ? (
                  <Actions disabled={busy} actions={[{ id: "item-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.itemCommand(i.id, i.version, "activate"), `${i.code} active`) }]} />
                ) : i.status === "active" ? (
                  <Actions disabled={busy} actions={[{ id: "item-deactivate", label: "Deactivate", onClick: () => void run(() => api.itemCommand(i.id, i.version, "deactivate"), `${i.code} inactive`) }]} />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={4}
        title="Stores"
        data-slice="8D"
        data-stores
        action={
          <Button size="sm" variant="ghost" data-action="store-new" disabled={busy} onClick={newStore}>
            New store
          </Button>
        }
      >
        <DataTable
          caption="Stores"
          rows={stores.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={Package} title="No stores" compact>Create a store to receive stock.</EmptyState>}
          columns={[
            { key: "name", header: "Store", render: (s) => <span style={{ paddingLeft: s.parentId ? 22 : 0, display: "inline-block" }}><CodeName name={s.name} code={s.code} /></span> },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Stock on hand" data-slice="8D" data-stock>
        <DataTable
          caption="Stock on hand"
          rows={stock.data ?? []}
          rowKey={(l) => `${l.itemId}|${l.locationId}`}
          empty={<EmptyState icon={Boxes} title="No stock" compact>Accepted goods receipts put stock here.</EmptyState>}
          columns={[
            { key: "item", header: "Item", render: (l) => <CodeName name={l.itemName} code={l.itemCode} sub={l.locationCode} /> },
            { key: "qty", header: "On hand", render: (l) => `${formatNumber(l.onHand)} ${l.unit}` },
            { key: "reorder", header: "Reorder", render: (l) => (l.atReorderLevel ? <StatusBadge status="reorder" /> : formatNumber(l.reorderLevel)) },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Replenishment needs"
        data-slice="8D"
        data-replenishment
        action={
          <Button size="sm" variant="ghost" data-action="replenishment-run" disabled={busy} onClick={runReplenishment}>
            Run reorder check
          </Button>
        }
      >
        <DataTable
          caption="Replenishment needs"
          rows={needs.data ?? []}
          rowKey={(n) => n.id}
          empty={<EmptyState icon={ShieldAlert} title="Nothing to replenish" compact>Run the reorder-level check.</EmptyState>}
          columns={[
            { key: "item", header: "Item", render: (n) => <CodeName name={n.itemName} code={n.itemCode} /> },
            { key: "onHand", header: "On hand", render: (n) => formatNumber(n.onHand), muted: true },
            { key: "suggested", header: "Order", render: (n) => formatNumber(n.suggestedQuantity) },
            { key: "status", header: "Status", render: (n) => <StatusBadge status={n.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Stock issues"
        data-slice="8D"
        data-issues
        action={
          <Button size="sm" variant="ghost" data-action="issue-new" disabled={busy} onClick={newIssue}>
            New issue
          </Button>
        }
      >
        <DataTable
          caption="Stock issues"
          rows={issues.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={ClipboardList} title="No issues" compact>Request stock for a department or a person.</EmptyState>}
          columns={[
            { key: "number", header: "No.", render: (s) => s.number },
            { key: "item", header: "Item", render: (s) => <CodeName name={s.itemCode} sub={s.locationCode} /> },
            { key: "qty", header: "Quantity", render: (s) => (!/^0(\.0+)?$/.test(s.returnedQuantity) ? `${formatNumber(s.quantity)} (returned ${formatNumber(s.returnedQuantity)})` : formatNumber(s.quantity)) },
            { key: "purpose", header: "Purpose", render: (s) => s.purpose, muted: true },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "actions",
              header: "",
              render: (s) => {
                const acts: RowAction[] = [];
                if (s.status === "requested") {
                  acts.push({ id: "issue-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideIssue(s.id, s.version, "approve"), `${s.number} approved`) });
                  acts.push({ id: "issue-reject", label: "Reject", onClick: () => ask({ title: `Reject · ${s.number}`, name: "issue-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideIssue(s.id, s.version, "reject", v.reason), `${s.number} rejected`) }) });
                }
                if (s.status === "approved") acts.push({ id: "issue-issue", label: "Hand over", variant: "primary", onClick: () => void run(() => api.issueStock(s.id, s.version), `${s.number} issued`) });
                if (s.status === "issued")
                  acts.push({
                    id: "issue-return",
                    label: "Return",
                    onClick: () =>
                      ask({
                        title: `Return to store · ${s.number}`,
                        name: "issue-return",
                        submitLabel: "Return",
                        fields: [
                          { name: "quantity", label: "Quantity", required: true, initial: toInvoice(s.quantity, s.returnedQuantity) },
                          { name: "reason", label: "Reason", wide: true },
                        ],
                        onSubmit: (v) => submit(() => api.returnStock(s.id, s.version, v.quantity ?? "", v.reason || undefined), (x) => `${x.number}: ${x.returnedQuantity} returned`),
                      }),
                  });
                return <Actions disabled={busy} actions={acts} />;
              },
            },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Reservations"
        data-slice="8D"
        data-reservations
        action={
          <Button size="sm" variant="ghost" data-action="reservation-new" disabled={busy} onClick={newReservation}>
            Reserve
          </Button>
        }
      >
        <DataTable
          caption="Reservations"
          rows={reservations.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={Package} title="No reservations" compact>Hold stock for an upcoming need.</EmptyState>}
          columns={[
            { key: "item", header: "Item", render: (r) => <CodeName name={r.itemCode} sub={r.locationCode} /> },
            { key: "qty", header: "Quantity", render: (r) => formatNumber(r.quantity) },
            { key: "purpose", header: "Purpose", render: (r) => r.purpose, muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            {
              key: "actions",
              header: "",
              render: (r) => (r.status === "active" ? <Actions disabled={busy} actions={[{ id: "reservation-release", label: "Release", onClick: () => void run(() => api.releaseReservation(r.id, r.version), "Reservation released") }]} /> : null),
            },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Adjustments"
        data-slice="8D"
        data-adjustments
        action={
          <Button size="sm" variant="ghost" data-action="adjustment-new" disabled={busy} onClick={newAdjustment}>
            Adjust
          </Button>
        }
      >
        <DataTable
          caption="Adjustments"
          rows={adjustments.data ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState icon={Wrench} title="No adjustments" compact>Correct stock after a count, with approval.</EmptyState>}
          columns={[
            { key: "item", header: "Item", render: (a) => <CodeName name={a.itemCode} sub={a.locationCode} /> },
            { key: "qty", header: "Quantity", render: (a) => formatNumber(a.quantity) },
            { key: "reason", header: "Reason", render: (a) => a.reason, muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
            {
              key: "actions",
              header: "",
              render: (a) =>
                a.status === "requested" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      { id: "adjustment-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideAdjustment(a.id, a.version, "approve"), "Adjustment applied") },
                      { id: "adjustment-reject", label: "Reject", onClick: () => ask({ title: "Reject adjustment", name: "adjustment-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideAdjustment(a.id, a.version, "reject", v.reason), "Adjustment rejected") }) },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================================= assets

const CONDITIONS = ["new", "good", "fair", "poor", "damaged"].map((c) => ({ value: c, label: statusLabel(c) }));

/** Admin → Campus → Assets. */
export function AssetsAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <Assets institution={institution} />;
}

function Assets({ institution }: { institution: Institution }) {
  const api = useResourcesApi();
  const inst = institution.id;
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const assets = useQuery({ queryKey: rkeys.scoped(inst, "assets"), queryFn: () => api.assets(inst) });
  const count = (f: (a: Asset) => boolean) => (assets.data ? String(assets.data.filter(f).length) : "…");
  // Custodians by name: rooms / spaces of the 8A tree and Domain E staff records (never raw ids).
  const facilities = useFacilitiesApi();
  const workforce = useWorkforceApi();
  const spaces = (useQuery({ queryKey: fkeys(inst, "tree"), queryFn: () => facilities.tree(inst) }).data ?? []).filter((n) => n.nodeType === "space");
  const staff = useQuery({ queryKey: wkeys.scoped(inst, "staff"), queryFn: () => workforce.staff(inst) }).data ?? [];
  const custodian = (a: Asset) => {
    if (!a.custodianKind) return "—";
    if (a.custodianKind === "space") {
      const n = spaces.find((x) => x.id === a.custodianId);
      return n ? `${n.code} · ${n.name}` : "Room / space";
    }
    const m = staff.find((x) => x.id === a.custodianId);
    return m ? `${m.displayName} · ${m.staffNumber}` : statusLabel(a.custodianKind);
  };
  const custodianOptions = [
    ...spaces.filter((n) => n.status === "active").map((n) => ({ value: `space:${n.id}`, label: `Room · ${n.code} · ${n.name}` })),
    ...staff.filter((m) => m.status === "active").map((m) => ({ value: `staff:${m.id}`, label: `Staff · ${m.displayName} · ${m.staffNumber}` })),
  ];

  const register = () =>
    ask({
      title: "Register asset",
      name: "asset-create",
      submitLabel: "Register",
      fields: [
        { name: "name", label: "Name", required: true, wide: true },
        { name: "serialNumber", label: "Serial number" },
        { name: "acquisitionCost", label: "Acquisition cost (₹)" },
        { name: "goodsReceiptLineId", label: "Goods receipt line id (optional)", wide: true },
      ],
      onSubmit: (v) =>
        submit(
          () => api.createAsset(inst, { name: v.name ?? "", serialNumber: v.serialNumber || undefined, acquisitionCost: v.acquisitionCost || undefined, goodsReceiptLineId: v.goodsReceiptLineId || undefined }),
          (a) => `${a.number} registered`,
        ),
    });
  const onAsset = (a: Asset, act: ReturnType<typeof assetActions>[number]) => {
    switch (act) {
      case "assign":
        return ask({
          title: `Assign · ${a.number}`,
          name: "asset-assign",
          submitLabel: "Assign",
          fields: [{ name: "assignee", label: "Custodian", type: "select", required: true, wide: true, options: custodianOptions, initial: custodianOptions[0]?.value }],
          onSubmit: (v) => {
            const [kind = "", id = ""] = (v.assignee ?? "").split(":");
            return submit(() => api.assignAsset(a.id, a.version, { assigneeKind: kind === "space" ? "space" : "staff", assigneeId: id }), `${a.number} assigned`);
          },
        });
      case "return":
        return ask({ title: `Return · ${a.number}`, name: "asset-return", submitLabel: "Return", fields: [{ name: "condition", label: "Condition", type: "select", options: CONDITIONS, initial: a.condition }], onSubmit: (v) => submit(() => api.returnAsset(a.id, a.version, v.condition), `${a.number} returned`) });
      case "condition":
        return ask({
          title: `Condition · ${a.number}`,
          name: "asset-condition",
          submitLabel: "Save",
          fields: [
            { name: "condition", label: "Condition", type: "select", options: CONDITIONS, initial: a.condition },
            { name: "repair", label: a.status === "under_repair" ? "Still under repair" : "Send for repair", type: "checkbox", initial: a.status === "under_repair" ? "true" : "false" },
            { name: "reason", label: "Note", wide: true },
          ],
          onSubmit: (v) => submit(() => api.assetCondition(a.id, a.version, { condition: v.condition ?? a.condition, repair: v.repair === "true", reason: v.reason || undefined }), (x) => `${x.number}: ${statusLabel(x.condition)} · ${statusLabel(x.status)}`),
        });
      case "retire":
        return ask({ title: `Retire · ${a.number}`, name: "asset-retire", submitLabel: "Retire", fields: [REASON], onSubmit: (v) => submit(() => api.retireAsset(a.id, a.version, v.reason ?? ""), (x) => (x.retirementPending ? `${x.number}: retirement awaits approval` : `${x.number} retired`)) });
      case "approve":
        return run(() => api.decideAsset(a.id, a.version, "approve"), `${a.number} retired`);
      case "reject":
        return ask({ title: `Keep in service · ${a.number}`, name: "asset-reject", submitLabel: "Reject retirement", fields: [REASON], onSubmit: (v) => submit(() => api.decideAsset(a.id, a.version, "reject", v.reason), `${a.number} stays in service`) });
    }
  };

  return (
    <>
      <KpiTile icon={Package} tone="orange" label="Assets in service" value={count((a) => a.status !== "retired")} sub="Registered, available, assigned" data-kpi="asset-total" />
      <KpiTile icon={PackageCheck} tone="green" label="Assigned" value={count((a) => a.status === "assigned")} sub="In someone's custody" data-kpi="asset-assigned" />
      <KpiTile icon={Wrench} tone="amber" label="Under repair" value={count((a) => a.status === "under_repair")} sub="Out of service" data-kpi="asset-repair" />
      <KpiTile icon={ShieldAlert} tone="grey" label="Retired" value={count((a) => a.status === "retired")} sub="Written off" data-kpi="asset-retired" />
      <BentoTile
        span={12}
        title="Asset register"
        data-slice="8D"
        data-assets
        action={
          <Button size="sm" variant="ghost" data-action="asset-new" disabled={busy} onClick={register}>
            Register asset
          </Button>
        }
      >
        <DataTable
          caption="Assets"
          rows={assets.data ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState icon={Package} title="No assets" compact>Register assets manually or from accepted goods receipts.</EmptyState>}
          columns={[
            { key: "name", header: "Asset", render: (a) => <CodeName name={a.name} code={a.number} /> },
            { key: "serial", header: "Serial", render: (a) => dash(a.serialNumber), muted: true },
            { key: "cost", header: "Cost", render: (a) => inr(a.acquisitionCost) },
            { key: "custody", header: "Custody", render: (a) => custodian(a), muted: true },
            { key: "condition", header: "Condition", render: (a) => statusLabel(a.condition) },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.retirementPending ? "pending_approval" : a.status} /> },
            {
              key: "actions",
              header: "",
              render: (a) => <Actions disabled={busy} actions={assetActions(a).map((x): RowAction => ({ id: `asset-${x}`, label: statusLabel(x), variant: x === "assign" || x === "approve" ? "primary" : "ghost", onClick: () => void onAsset(a, x) }))} />,
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
