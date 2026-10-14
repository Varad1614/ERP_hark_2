/**
 * In-memory slice-8D API for mock mode and tests: one active supplier, an active consumable
 * (`PAPER-A4`) with stock in the central store, a released requisition and one registered asset.
 * Commands move statuses the way the API does (enough for screens and tests; the API stays
 * authoritative). Decimal values stay strings; the little arithmetic the mock needs works in
 * thousandths so no float ever reaches a screen.
 */
import { ApiProblem } from "@sutra/api-client";
import type { ResourcesApi } from "./api";
import type {
  Adjustment,
  Asset,
  GoodsReceipt,
  InvoiceFinding,
  Item,
  PurchaseOrder,
  ReplenishmentNeed,
  Requisition,
  Reservation,
  SourcingEvent,
  StockIssue,
  StoreLocation,
  Supplier,
  VendorInvoice,
  VendorPayment,
} from "./types";

const problem = (status: number, title: string, errorCode: string) =>
  new ApiProblem({ type: "about:blank", title, status, errorCode } as ConstructorParameters<typeof ApiProblem>[0]);
const conflict = (title: string) => problem(409, title, "conflict");
const notFound = () => problem(404, "Not found", "not-found");

let seq = 100;
const nextId = (p: string) => `${p}-${++seq}`;
const pad = (n: number) => String(n).padStart(6, "0");

/** Decimal string → integer thousandths (and back), for the mock's own sums. */
export function milli(v: string): number {
  const [i = "0", f = ""] = v.trim().replace("-", "").split(".");
  const n = Number(i) * 1000 + Number((f + "000").slice(0, 3));
  return v.trim().startsWith("-") ? -n : n;
}
export function fromMilli(n: number, scale: 2 | 3): string {
  const sign = n < 0 ? "-" : "";
  const a = Math.abs(n);
  const whole = Math.floor(a / 1000);
  const frac = String(a % 1000).padStart(3, "0").slice(0, scale);
  return `${sign}${whole}.${frac}`;
}
const invoiceLines = new Map<string, Array<{ lineNo: number; quantity: string; unitPrice: string }>>();
const paymentAllocations = new Map<string, Array<{ vendorInvoiceId: string; amount: string }>>();
const money = (q: string, p: string) => fromMilli(Math.round((milli(q) * milli(p)) / 1000), 2);

export function createMockResourcesApi(): ResourcesApi {
  const suppliers: Supplier[] = [
    { id: "sup-1", institutionId: "inst-1", number: "SUP-000001", legalName: "Acme Traders", gstin: "27ABCDE1234F1Z5", pan: "ABCDE1234F", bankAccountNumber: "XXXXXX7890", bankIfsc: "HDFC0001234", status: "active", approvalPending: false, version: 3 },
  ];
  const items: Item[] = [
    { id: "itm-1", code: "PAPER-A4", name: "A4 paper (ream)", kind: "consumable", unit: "ream", reorderLevel: "12.000", standardCost: "250.00", status: "active", onHand: "40.000", reserved: "0.000", version: 2 },
  ];
  const stores: StoreLocation[] = [{ id: "loc-1", parentId: null, code: "CENTRAL", name: "Central store", status: "active", version: 1 }];
  const onHand = new Map<string, number>([["itm-1|loc-1", 40000]]);
  const requisitions: Requisition[] = [
    {
      id: "req-1",
      institutionId: "inst-1",
      number: "PR-000001",
      title: "Exam stationery",
      revision: 1,
      estimatedTotal: "5000.00",
      status: "released",
      source: "manual",
      version: 4,
      lines: [{ lineNo: 1, itemId: "itm-1", itemCode: "PAPER-A4", description: "A4 paper (ream)", quantity: "20.000", unit: "ream", estimatedUnitCost: "250.00", estimatedAmount: "5000.00" }],
    },
  ];
  const events: SourcingEvent[] = [];
  const orders: PurchaseOrder[] = [];
  const receipts: GoodsReceipt[] = [];
  const invoices: VendorInvoice[] = [];
  const payments: VendorPayment[] = [];
  const issues: StockIssue[] = [];
  const reservations: Reservation[] = [];
  const adjustments: Adjustment[] = [];
  const needs: ReplenishmentNeed[] = [];
  const assets: Asset[] = [{ id: "ast-1", number: "AST-000001", name: "Projector", serialNumber: "PJ-1", acquisitionCost: "30000.00", condition: "good", status: "registered", retirementPending: false, version: 1 }];

  const find = <T extends { id: string }>(list: T[], id: string): T => {
    const x = list.find((r) => r.id === id);
    if (!x) throw notFound();
    return x;
  };
  const touch = <T extends { version: number }>(x: T, patch: Partial<T>): T => Object.assign(x, patch, { version: x.version + 1 });
  const check = (x: { version: number }, v: number) => {
    if (x.version !== v) throw problem(412, "Version conflict", "version-conflict");
  };
  const stockOf = (item: string, loc: string) => onHand.get(`${item}|${loc}`) ?? 0;
  const reservedOf = (item: string, loc: string) => reservations.filter((r) => r.status === "active" && r.itemCode === find(items, item).code && r.locationCode === find(stores, loc).code).reduce((s, r) => s + milli(r.quantity), 0);
  const move = (item: string, loc: string, delta: number) => {
    const next = stockOf(item, loc) + delta;
    if (next < 0) throw conflict("insufficient stock");
    onHand.set(`${item}|${loc}`, next);
    const it = find(items, item);
    it.onHand = fromMilli(milli(it.onHand) + delta, 3);
  };
  const issueLoc = new Map<string, { item: string; loc: string }>();

  return {
    suppliers: async () => suppliers.map((s) => ({ ...s })),
    createSupplier: async (_i, input) => {
      const s: Supplier = { id: nextId("sup"), institutionId: _i, number: `SUP-${pad(suppliers.length + 1)}`, legalName: input.legalName, gstin: input.gstin ?? null, pan: input.pan ?? null, bankAccountNumber: input.bankAccountNumber ? `XXXXXX${input.bankAccountNumber.slice(-4)}` : null, status: "prospective", approvalPending: false, version: 1 };
      suppliers.push(s);
      return { ...s };
    },
    supplierCommand: async (id, v, cmd, reason) => {
      const s = find(suppliers, id);
      check(s, v);
      const to: Record<string, [string, string]> = { register: ["prospective", "registered"], submit: ["registered", "active"], suspend: ["active", "suspended"], restore: ["suspended", "active"] };
      const [from, next] = to[cmd] ?? ["", ""];
      if (s.status !== from) throw conflict(`supplier ${s.number} is ${s.status}`);
      return { ...touch(s, { status: next, statusReason: reason ?? null }) };
    },
    decideSupplier: async (id, v, d) => {
      const s = find(suppliers, id);
      check(s, v);
      return { ...touch(s, { status: d === "approve" ? "active" : s.status, approvalPending: false }) };
    },
    requisitions: async () => requisitions.map((r) => ({ ...r })),
    createRequisition: async (_i, input) => {
      const lines = input.lines.map((l, n) => {
        const it = l.itemId ? find(items, l.itemId) : null;
        const cost = l.estimatedUnitCost ?? it?.standardCost ?? "0.00";
        return { lineNo: n + 1, itemId: it?.id ?? null, itemCode: it?.code ?? null, description: l.description ?? it?.name ?? "", quantity: fromMilli(milli(l.quantity), 3), unit: it?.unit ?? "each", estimatedUnitCost: cost, estimatedAmount: money(l.quantity, cost) };
      });
      const total = fromMilli(lines.reduce((s, l) => s + milli(l.estimatedAmount), 0), 2);
      const r: Requisition = { id: nextId("req"), institutionId: _i, number: `PR-${pad(requisitions.length + 1)}`, title: input.title, revision: 1, estimatedTotal: total, status: "draft", source: "manual", version: 1, lines };
      requisitions.push(r);
      return { ...r };
    },
    submitRequisition: async (id, v) => {
      const r = find(requisitions, id);
      check(r, v);
      if (!["draft", "amended"].includes(r.status)) throw conflict(`requisition ${r.number} is ${r.status}`);
      return { ...touch(r, { status: "released" }) };
    },
    decideRequisition: async (id, v, d, reason) => {
      const r = find(requisitions, id);
      check(r, v);
      if (r.status !== "submitted") throw conflict(`requisition ${r.number} is ${r.status}`);
      return { ...touch(r, { status: d === "approve" ? "released" : "rejected", statusReason: reason ?? null }) };
    },
    cancelRequisition: async (id, v, reason) => {
      const r = find(requisitions, id);
      check(r, v);
      if (["ordered", "closed", "cancelled"].includes(r.status)) throw conflict(`requisition ${r.number} is ${r.status}`);
      return { ...touch(r, { status: "cancelled", statusReason: reason }) };
    },
    sourcingEvents: async () => events.map((e) => ({ ...e })),
    sourcingEvent: async (id) => ({ ...find(events, id) }),
    createSourcingEvent: async (_i, input) => {
      const r = find(requisitions, input.requisitionId);
      if (r.status !== "released") throw conflict(`requisition ${r.number} is ${r.status}`);
      if (events.some((e) => e.requisitionId === r.id && e.status !== "cancelled")) throw conflict("already being sourced");
      const e: SourcingEvent = { id: nextId("rfq"), number: `RFQ-${pad(events.length + 1)}`, requisitionId: r.id, requisitionNumber: r.number, method: input.method, title: r.title, estimatedValue: r.estimatedTotal, status: "open", version: 1, lines: r.lines, quotations: [] };
      events.push(e);
      return { ...e };
    },
    quote: async (id, v, input) => {
      const e = find(events, id);
      check(e, v);
      if (e.status !== "open") throw conflict(`sourcing event ${e.number} is ${e.status}`);
      const s = find(suppliers, input.supplierId);
      if (s.status !== "active") throw conflict(`supplier ${s.number} is ${s.status}`);
      const lines = input.lines.map((l) => ({ lineNo: l.lineNo, unitPrice: l.unitPrice, amount: money(e.lines?.find((d) => d.lineNo === l.lineNo)?.quantity ?? "0", l.unitPrice) }));
      const total = fromMilli(lines.reduce((sum, l) => sum + milli(l.amount), 0), 2);
      const quotations = [...(e.quotations ?? []), { id: nextId("quo"), supplierId: s.id, supplierNumber: s.number, supplierName: s.legalName, totalAmount: total, rank: null, status: "received", lines }];
      return { ...touch(e, { quotations }) };
    },
    evaluate: async (id, v) => {
      const e = find(events, id);
      check(e, v);
      if (!e.quotations?.length) throw conflict("no quotation to evaluate");
      const ranked = [...e.quotations].sort((a, b) => milli(a.totalAmount) - milli(b.totalAmount)).map((q, n) => ({ ...q, rank: n + 1, status: "shortlisted" }));
      return { ...touch(e, { status: "evaluating", quotations: ranked }) };
    },
    award: async (id, v, quotationId) => {
      const e = find(events, id);
      check(e, v);
      const q = e.quotations?.find((x) => x.id === quotationId);
      if (!q) throw notFound();
      const quotations = (e.quotations ?? []).map((x) => ({ ...x, status: x.id === quotationId ? "accepted" : "rejected" }));
      return { ...touch(e, { status: "awarded", awardedQuotationId: q.id, awardedSupplierId: q.supplierId, quotations }) };
    },
    purchaseOrders: async () => orders.map((o) => ({ ...o })),
    purchaseOrder: async (id) => ({ ...find(orders, id) }),
    createPurchaseOrder: async (inst, input) => {
      const e = input.sourcingEventId ? find(events, input.sourcingEventId) : null;
      const r = find(requisitions, e?.requisitionId ?? input.requisitionId ?? "");
      if (r.status !== "released") throw conflict(`requisition ${r.number} is ${r.status}`);
      const supplierId = e?.awardedSupplierId ?? input.supplierId ?? "";
      const s = find(suppliers, supplierId);
      const q = e?.quotations?.find((x) => x.id === e.awardedQuotationId);
      const lines = (r.lines ?? []).map((l) => {
        const price = q?.lines?.find((x) => x.lineNo === l.lineNo)?.unitPrice ?? l.estimatedUnitCost;
        const kind = l.itemId ? (find(items, l.itemId).kind === "asset" ? "asset" : "stock") : "service";
        return { id: nextId("pol"), lineNo: l.lineNo, itemId: l.itemId ?? null, itemCode: l.itemCode ?? null, lineKind: kind, description: l.description, quantity: l.quantity, unit: l.unit, unitPrice: price, amount: money(l.quantity, price), receivedQuantity: "0.000", invoicedQuantity: "0.000" };
      });
      const o: PurchaseOrder = { id: nextId("po"), institutionId: inst, number: `PO-${pad(orders.length + 1)}`, supplierId: s.id, supplierNumber: s.number, supplierName: s.legalName, requisitionId: r.id, sourcingEventId: e?.id ?? null, deliveryLocationId: input.deliveryLocationId ?? "loc-1", orderDate: "2026-11-01", revision: 1, totalAmount: fromMilli(lines.reduce((sum, l) => sum + milli(l.amount), 0), 2), status: "draft", approvalPending: false, version: 1, lines };
      orders.push(o);
      return { ...o };
    },
    issueOrder: async (id, v) => {
      const o = find(orders, id);
      check(o, v);
      if (o.status !== "draft") throw conflict(`purchase order ${o.number} is ${o.status}`);
      const r = requisitions.find((x) => x.id === o.requisitionId);
      if (r && r.status === "released") touch(r, { status: "ordered" });
      return { ...touch(o, { status: "issued" }) };
    },
    decideOrder: async (id, v, d) => {
      const o = find(orders, id);
      check(o, v);
      return { ...touch(o, { status: d === "approve" ? "issued" : o.status, approvalPending: false }) };
    },
    acknowledgeOrder: async (id, v, reference) => {
      const o = find(orders, id);
      check(o, v);
      if (o.status !== "issued") throw conflict(`purchase order ${o.number} is ${o.status}`);
      return { ...touch(o, { status: "acknowledged", acknowledgementRef: reference }) };
    },
    cancelOrder: async (id, v) => {
      const o = find(orders, id);
      check(o, v);
      if (receipts.some((g) => g.purchaseOrderId === o.id)) throw conflict(`purchase order ${o.number} has goods receipts`);
      const r = requisitions.find((x) => x.id === o.requisitionId);
      if (r && r.status === "ordered") touch(r, { status: "released" });
      return { ...touch(o, { status: "cancelled" }) };
    },
    closeOrder: async (id, v) => {
      const o = find(orders, id);
      check(o, v);
      if (o.status !== "received" || (o.lines ?? []).some((l) => milli(l.invoicedQuantity) < milli(l.receivedQuantity))) throw conflict(`purchase order ${o.number} is not fully received and invoiced`);
      return { ...touch(o, { status: "closed" }) };
    },
    receipts: async () => receipts.map((g) => ({ ...g })),
    receive: async (poId, input) => {
      const o = find(orders, poId);
      if (!["issued", "acknowledged", "partially_received"].includes(o.status)) throw conflict(`purchase order ${o.number} is ${o.status}`);
      const lines = input.lines.map((l, n) => {
        const pl = (o.lines ?? []).find((x) => x.lineNo === l.lineNo);
        if (!pl) throw notFound();
        if (milli(l.quantity) > milli(pl.quantity) - milli(pl.receivedQuantity)) throw problem(422, "over-receipt", "validation");
        return { id: nextId("grl"), lineNo: n + 1, purchaseOrderLineNo: pl.lineNo, lineKind: pl.lineKind, description: pl.description, receivedQuantity: fromMilli(milli(l.quantity), 3), acceptedQuantity: "0.000", rejectedQuantity: "0.000", assetsRegistered: 0 };
      });
      const loc = find(stores, input.locationId ?? o.deliveryLocationId ?? "loc-1");
      const g: GoodsReceipt = { id: nextId("grn"), number: `GRN-${pad(receipts.length + 1)}`, purchaseOrderId: o.id, purchaseOrderNumber: o.number, supplierNumber: o.supplierNumber, locationCode: loc.code, receivedOn: "2026-11-05", status: "received", version: 1, lines };
      receipts.push(g);
      return { ...g };
    },
    acceptReceipt: async (id, v) => {
      const g = find(receipts, id);
      check(g, v);
      if (!["received", "inspecting"].includes(g.status)) throw conflict(`goods receipt ${g.number} is ${g.status}`);
      const o = find(orders, g.purchaseOrderId);
      const loc = stores.find((s) => s.code === g.locationCode) ?? stores[0];
      for (const l of g.lines ?? []) {
        l.acceptedQuantity = l.receivedQuantity;
        const pl = (o.lines ?? []).find((x) => x.lineNo === l.purchaseOrderLineNo);
        if (!pl) continue;
        pl.receivedQuantity = fromMilli(milli(pl.receivedQuantity) + milli(l.receivedQuantity), 3);
        if (pl.lineKind === "stock" && pl.itemId && loc) move(pl.itemId, loc.id, milli(l.receivedQuantity));
      }
      const all = (o.lines ?? []).every((l) => milli(l.receivedQuantity) >= milli(l.quantity));
      touch(o, { status: all ? "received" : "partially_received" });
      return { ...touch(g, { status: "accepted" }) };
    },
    rejectReceipt: async (id, v) => {
      const g = find(receipts, id);
      check(g, v);
      return { ...touch(g, { status: "rejected" }) };
    },
    invoices: async () => invoices.map((x) => ({ ...x })),
    createInvoice: async (_i, input) => {
      const o = find(orders, input.purchaseOrderId);
      const total = input.lines.reduce((s, l) => s + milli(money(l.quantity, l.unitPrice)), 0) * (input.creditNote ? -1 : 1);
      const inv: VendorInvoice = { id: nextId("vinv"), number: `VINV-${pad(invoices.length + 1)}`, supplierId: o.supplierId, supplierNumber: o.supplierNumber, supplierName: o.supplierName, purchaseOrderId: o.id, purchaseOrderNumber: o.number, supplierInvoiceNumber: input.supplierInvoiceNumber, isCreditNote: !!input.creditNote, invoiceDate: input.invoiceDate, totalAmount: fromMilli(total, 2), paidAmount: "0.00", status: "received", matchFindings: [], approvalPending: false, version: 1 };
      invoiceLines.set(inv.id, input.lines);
      invoices.push(inv);
      return { ...inv };
    },
    matchInvoice: async (id, v) => {
      const inv = find(invoices, id);
      check(inv, v);
      const o = find(orders, inv.purchaseOrderId);
      const findings = (invoiceLines.get(id) ?? []).flatMap((l) => {
        const pl = (o.lines ?? []).find((x) => x.lineNo === l.lineNo);
        const out: InvoiceFinding[] = [];
        if (!pl) return out;
        if (!inv.isCreditNote && milli(l.quantity) + milli(pl.invoicedQuantity) > milli(pl.receivedQuantity)) out.push({ lineNo: l.lineNo, code: "quantity_exceeds_received", message: `only ${pl.receivedQuantity} accepted` });
        if (Math.abs(milli(l.unitPrice) - milli(pl.unitPrice)) > Math.round(milli(pl.unitPrice) / 100)) out.push({ lineNo: l.lineNo, code: "price_variance", message: `order price ${pl.unitPrice}` });
        return out;
      });
      return { ...touch(inv, { status: findings.length ? "mismatch" : "approved", matchFindings: findings }) };
    },
    decideInvoice: async (id, v, d) => {
      const inv = find(invoices, id);
      check(inv, v);
      return { ...touch(inv, { status: d === "approve" ? "approved" : "cancelled", approvalPending: false }) };
    },
    postInvoice: async (id, v) => {
      const inv = find(invoices, id);
      check(inv, v);
      if (inv.status !== "approved") throw conflict(`vendor invoice ${inv.number} is ${inv.status}; only a matched and approved invoice posts`);
      const o = find(orders, inv.purchaseOrderId);
      for (const l of invoiceLines.get(id) ?? []) {
        const pl = (o.lines ?? []).find((x) => x.lineNo === l.lineNo);
        if (pl) pl.invoicedQuantity = fromMilli(milli(pl.invoicedQuantity) + milli(l.quantity) * (inv.isCreditNote ? -1 : 1), 3);
      }
      return { ...touch(inv, { status: "posted", entryNumber: `JE-${pad(invoices.length + payments.length)}` }) };
    },
    payments: async () => payments.map((p) => ({ ...p })),
    createPayment: async (_i, input) => {
      const s = find(suppliers, input.supplierId);
      for (const a of input.allocations) {
        const inv = find(invoices, a.vendorInvoiceId);
        if (inv.status !== "posted") throw conflict(`vendor invoice ${inv.number} is ${inv.status}`);
        if (milli(a.amount) > milli(inv.totalAmount) - milli(inv.paidAmount)) throw problem(422, "exceeds outstanding", "validation");
      }
      const p: VendorPayment = { id: nextId("vpay"), number: `VPAY-${pad(payments.length + 1)}`, supplierId: s.id, supplierNumber: s.number, paymentDate: input.paymentDate, amount: fromMilli(input.allocations.reduce((sum, a) => sum + milli(a.amount), 0), 2), status: "draft", version: 1 };
      paymentAllocations.set(p.id, input.allocations);
      payments.push(p);
      return { ...p };
    },
    postPayment: async (id, v) => {
      const p = find(payments, id);
      check(p, v);
      if (p.status !== "draft") throw conflict(`vendor payment ${p.number} is ${p.status}`);
      for (const a of paymentAllocations.get(id) ?? []) {
        const inv = find(invoices, a.vendorInvoiceId);
        const paid = milli(inv.paidAmount) + milli(a.amount);
        touch(inv, { paidAmount: fromMilli(paid, 2), status: paid >= milli(inv.totalAmount) ? "paid" : inv.status });
      }
      return { ...touch(p, { status: "posted", entryNumber: `JE-${pad(invoices.length + payments.length + 1)}` }) };
    },
    items: async () => items.map((i) => ({ ...i })),
    createItem: async (_i, input) => {
      const code = input.code.trim().toUpperCase();
      if (items.some((i) => i.code === code)) throw conflict(`item code ${code} is already in use`);
      const it: Item = { id: nextId("itm"), code, name: input.name, kind: input.kind, unit: input.unit ?? "each", reorderLevel: input.reorderLevel ? fromMilli(milli(input.reorderLevel), 3) : null, standardCost: input.standardCost ?? null, status: "draft", onHand: "0.000", reserved: "0.000", version: 1 };
      items.push(it);
      return { ...it };
    },
    itemCommand: async (id, v, cmd) => {
      const it = find(items, id);
      check(it, v);
      return { ...touch(it, { status: cmd === "activate" ? "active" : "inactive" }) };
    },
    stores: async () => stores.map((s) => ({ ...s })),
    createStore: async (_i, input) => {
      const s: StoreLocation = { id: nextId("loc"), parentId: null, code: input.code.trim().toUpperCase(), name: input.name, status: "active", version: 1 };
      stores.push(s);
      return { ...s };
    },
    stock: async () =>
      [...onHand.entries()].map(([k, q]) => {
        const [itemId = "", locationId = ""] = k.split("|");
        const it = find(items, itemId);
        const loc = find(stores, locationId);
        return { itemId, itemCode: it.code, itemName: it.name, locationId, locationCode: loc.code, onHand: fromMilli(q, 3), unit: it.unit, reorderLevel: it.reorderLevel ?? null, atReorderLevel: !!it.reorderLevel && q <= milli(it.reorderLevel) };
      }),
    issues: async () => issues.map((s) => ({ ...s })),
    createIssue: async (_i, input) => {
      const it = find(items, input.itemId);
      const loc = find(stores, input.locationId);
      const s: StockIssue = { id: nextId("siv"), number: `SIV-${pad(issues.length + 1)}`, itemId: it.id, itemCode: it.code, locationCode: loc.code, quantity: fromMilli(milli(input.quantity), 3), returnedQuantity: "0.000", purpose: input.purpose, status: "requested", version: 1 };
      issueLoc.set(s.id, { item: it.id, loc: loc.id });
      issues.push(s);
      return { ...s };
    },
    decideIssue: async (id, v, d) => {
      const s = find(issues, id);
      check(s, v);
      if (s.status !== "requested") throw conflict(`stock issue ${s.number} is ${s.status}`);
      return { ...touch(s, { status: d === "approve" ? "approved" : "cancelled" }) };
    },
    issueStock: async (id, v) => {
      const s = find(issues, id);
      check(s, v);
      if (s.status !== "approved") throw conflict(`stock issue ${s.number} is ${s.status}`);
      const at = issueLoc.get(id);
      if (!at) throw notFound();
      if (stockOf(at.item, at.loc) - reservedOf(at.item, at.loc) < milli(s.quantity)) throw conflict(`insufficient stock of ${s.itemCode}`);
      move(at.item, at.loc, -milli(s.quantity));
      return { ...touch(s, { status: "issued" }) };
    },
    returnStock: async (id, v, quantity) => {
      const s = find(issues, id);
      check(s, v);
      const out = milli(s.quantity) - milli(s.returnedQuantity);
      if (s.status !== "issued") throw conflict(`stock issue ${s.number} is ${s.status}`);
      if (milli(quantity) > out) throw problem(422, "exceeds issued", "validation");
      const at = issueLoc.get(id);
      if (at) move(at.item, at.loc, milli(quantity));
      const returned = milli(s.returnedQuantity) + milli(quantity);
      return { ...touch(s, { returnedQuantity: fromMilli(returned, 3), status: returned >= milli(s.quantity) ? "returned" : "issued" }) };
    },
    reservations: async () => reservations.map((r) => ({ ...r })),
    reserve: async (_i, input) => {
      const it = find(items, input.itemId);
      const loc = find(stores, input.locationId);
      if (stockOf(it.id, loc.id) - reservedOf(it.id, loc.id) < milli(input.quantity)) throw conflict(`insufficient stock of ${it.code}`);
      const r: Reservation = { id: nextId("res"), itemCode: it.code, locationCode: loc.code, quantity: fromMilli(milli(input.quantity), 3), purpose: input.purpose, status: "active", version: 1 };
      reservations.push(r);
      it.reserved = fromMilli(milli(it.reserved) + milli(input.quantity), 3);
      return { ...r };
    },
    releaseReservation: async (id, v) => {
      const r = find(reservations, id);
      check(r, v);
      if (r.status !== "active") throw conflict("reservation is not active");
      return { ...touch(r, { status: "released" }) };
    },
    adjustments: async () => adjustments.map((a) => ({ ...a })),
    adjust: async (_i, input) => {
      const it = find(items, input.itemId);
      const loc = find(stores, input.locationId);
      move(it.id, loc.id, milli(input.quantity));
      const a: Adjustment = { id: nextId("adj"), itemCode: it.code, locationCode: loc.code, quantity: fromMilli(milli(input.quantity), 3), reason: input.reason, status: "applied", version: 1 };
      adjustments.push(a);
      return { ...a };
    },
    decideAdjustment: async (id, v, d) => {
      const a = find(adjustments, id);
      check(a, v);
      return { ...touch(a, { status: d === "approve" ? "applied" : "rejected" }) };
    },
    replenishment: async () => needs.map((n) => ({ ...n })),
    runReplenishment: async (_i, draftRequisition) => {
      const created: string[] = [];
      let skipped = 0;
      for (const it of items) {
        if (it.status !== "active" || !it.reorderLevel) continue;
        const avail = milli(it.onHand) - milli(it.reserved);
        if (avail > milli(it.reorderLevel) || needs.some((n) => n.itemCode === it.code && n.status !== "closed")) {
          skipped += 1;
          continue;
        }
        const n: ReplenishmentNeed = { id: nextId("need"), itemCode: it.code, itemName: it.name, onHand: it.onHand, reorderLevel: it.reorderLevel, suggestedQuantity: it.reorderLevel, status: draftRequisition ? "requisitioned" : "open" };
        needs.push(n);
        created.push(n.id);
      }
      return { processed: created.length, skipped, createdIds: created };
    },
    assets: async () => assets.map((a) => ({ ...a })),
    createAsset: async (_i, input) => {
      if (input.serialNumber && assets.some((a) => a.serialNumber === input.serialNumber)) throw conflict("an asset with this serial number is already registered");
      const a: Asset = { id: nextId("ast"), number: `AST-${pad(assets.length + 1)}`, name: input.name, serialNumber: input.serialNumber ?? null, acquisitionCost: input.acquisitionCost ?? null, condition: "new", status: "registered", retirementPending: false, version: 1 };
      assets.push(a);
      return { ...a };
    },
    assignAsset: async (id, v, input) => {
      const a = find(assets, id);
      check(a, v);
      if (!["registered", "available"].includes(a.status)) throw conflict(`asset ${a.number} is ${a.status}`);
      return { ...touch(a, { status: "assigned", custodianKind: input.assigneeKind, custodianId: input.assigneeId }) };
    },
    returnAsset: async (id, v, condition) => {
      const a = find(assets, id);
      check(a, v);
      if (a.status !== "assigned") throw conflict(`asset ${a.number} is ${a.status}`);
      return { ...touch(a, { status: "available", custodianKind: null, custodianId: null, condition: condition ?? a.condition }) };
    },
    assetCondition: async (id, v, input) => {
      const a = find(assets, id);
      check(a, v);
      const status = input.repair === true ? "under_repair" : input.repair === false && a.status === "under_repair" ? "available" : a.status;
      return { ...touch(a, { condition: input.condition, status }) };
    },
    retireAsset: async (id, v) => {
      const a = find(assets, id);
      check(a, v);
      if (a.status === "assigned" || a.status === "retired") throw conflict(`asset ${a.number} is ${a.status}`);
      return { ...touch(a, { status: "retired" }) };
    },
    decideAsset: async (id, v, d) => {
      const a = find(assets, id);
      check(a, v);
      return { ...touch(a, { status: d === "approve" ? "retired" : a.status, retirementPending: false }) };
    },
  };
}
