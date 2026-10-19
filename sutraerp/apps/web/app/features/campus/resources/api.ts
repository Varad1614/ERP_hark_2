/**
 * Slice-8D API layer: one interface, an HTTP implementation (CSRF, Idempotency-Key on creates,
 * If-Match on commands, problem+json → `ApiProblem`) and an in-memory mock (`./mock`). Every path
 * template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  Adjustment,
  Asset,
  AssetInput,
  BulkResult,
  Decision,
  GoodsReceipt,
  Item,
  ItemInput,
  Items,
  PurchaseOrder,
  ReplenishmentNeed,
  Requisition,
  RequisitionLineInput,
  Reservation,
  SourcingEvent,
  StockIssue,
  StockLine,
  StoreLocation,
  Supplier,
  SupplierCommand,
  SupplierInput,
  VendorInvoice,
  VendorPayment,
} from "./types";

export interface ResourcesApi {
  suppliers(institutionId: string): Promise<Supplier[]>;
  createSupplier(institutionId: string, input: SupplierInput): Promise<Supplier>;
  supplierCommand(id: string, version: number, cmd: SupplierCommand, reason?: string): Promise<Supplier>;
  decideSupplier(id: string, version: number, decision: Decision, reason?: string): Promise<Supplier>;
  requisitions(institutionId: string): Promise<Requisition[]>;
  createRequisition(institutionId: string, input: { title: string; lines: RequisitionLineInput[] }): Promise<Requisition>;
  submitRequisition(id: string, version: number): Promise<Requisition>;
  decideRequisition(id: string, version: number, decision: Decision, reason?: string): Promise<Requisition>;
  cancelRequisition(id: string, version: number, reason: string): Promise<Requisition>;
  sourcingEvents(institutionId: string): Promise<SourcingEvent[]>;
  sourcingEvent(id: string): Promise<SourcingEvent>;
  createSourcingEvent(institutionId: string, input: { requisitionId: string; method: "rfq" | "direct" }): Promise<SourcingEvent>;
  quote(id: string, version: number, input: { supplierId: string; lines: Array<{ lineNo: number; unitPrice: string }> }): Promise<SourcingEvent>;
  evaluate(id: string, version: number): Promise<SourcingEvent>;
  award(id: string, version: number, quotationId: string, reason?: string): Promise<SourcingEvent>;
  purchaseOrders(institutionId: string): Promise<PurchaseOrder[]>;
  purchaseOrder(id: string): Promise<PurchaseOrder>;
  createPurchaseOrder(institutionId: string, input: { sourcingEventId?: string; requisitionId?: string; supplierId?: string; deliveryLocationId?: string }): Promise<PurchaseOrder>;
  issueOrder(id: string, version: number): Promise<PurchaseOrder>;
  decideOrder(id: string, version: number, decision: Decision, reason?: string): Promise<PurchaseOrder>;
  acknowledgeOrder(id: string, version: number, reference: string): Promise<PurchaseOrder>;
  cancelOrder(id: string, version: number, reason: string): Promise<PurchaseOrder>;
  closeOrder(id: string, version: number): Promise<PurchaseOrder>;
  receipts(institutionId: string): Promise<GoodsReceipt[]>;
  receive(purchaseOrderId: string, input: { locationId?: string; lines: Array<{ lineNo: number; quantity: string }> }): Promise<GoodsReceipt>;
  acceptReceipt(id: string, version: number): Promise<GoodsReceipt>;
  rejectReceipt(id: string, version: number, reason: string): Promise<GoodsReceipt>;
  invoices(institutionId: string): Promise<VendorInvoice[]>;
  createInvoice(
    institutionId: string,
    input: { purchaseOrderId: string; supplierInvoiceNumber: string; invoiceDate: string; creditNote?: boolean; lines: Array<{ lineNo: number; quantity: string; unitPrice: string }> },
  ): Promise<VendorInvoice>;
  matchInvoice(id: string, version: number): Promise<VendorInvoice>;
  decideInvoice(id: string, version: number, decision: Decision, reason?: string): Promise<VendorInvoice>;
  postInvoice(id: string, version: number): Promise<VendorInvoice>;
  payments(institutionId: string): Promise<VendorPayment[]>;
  createPayment(institutionId: string, input: { supplierId: string; paymentDate: string; reference?: string; allocations: Array<{ vendorInvoiceId: string; amount: string }> }): Promise<VendorPayment>;
  decidePayment(id: string, version: number, decision: Decision, reason?: string): Promise<VendorPayment>;
  postPayment(id: string, version: number): Promise<VendorPayment>;
  items(institutionId: string): Promise<Item[]>;
  createItem(institutionId: string, input: ItemInput): Promise<Item>;
  itemCommand(id: string, version: number, cmd: "activate" | "deactivate"): Promise<Item>;
  stores(institutionId: string): Promise<StoreLocation[]>;
  createStore(institutionId: string, input: { code: string; name: string }): Promise<StoreLocation>;
  stock(institutionId: string): Promise<StockLine[]>;
  issues(institutionId: string): Promise<StockIssue[]>;
  createIssue(institutionId: string, input: { itemId: string; locationId: string; quantity: string; purpose: string }): Promise<StockIssue>;
  decideIssue(id: string, version: number, decision: Decision, reason?: string): Promise<StockIssue>;
  issueStock(id: string, version: number): Promise<StockIssue>;
  returnStock(id: string, version: number, quantity: string, reason?: string): Promise<StockIssue>;
  reservations(institutionId: string): Promise<Reservation[]>;
  reserve(institutionId: string, input: { itemId: string; locationId: string; quantity: string; purpose: string }): Promise<Reservation>;
  releaseReservation(id: string, version: number, reason?: string): Promise<Reservation>;
  adjustments(institutionId: string): Promise<Adjustment[]>;
  adjust(institutionId: string, input: { itemId: string; locationId: string; quantity: string; reason: string }): Promise<Adjustment>;
  decideAdjustment(id: string, version: number, decision: Decision, reason?: string): Promise<Adjustment>;
  replenishment(institutionId: string): Promise<ReplenishmentNeed[]>;
  runReplenishment(institutionId: string, draftRequisition: boolean): Promise<BulkResult>;
  assets(institutionId: string): Promise<Asset[]>;
  createAsset(institutionId: string, input: AssetInput): Promise<Asset>;
  assignAsset(id: string, version: number, input: { assigneeKind: "staff" | "space"; assigneeId: string }): Promise<Asset>;
  returnAsset(id: string, version: number, condition?: string): Promise<Asset>;
  assetCondition(id: string, version: number, input: { condition: string; repair?: boolean; reason?: string }): Promise<Asset>;
  retireAsset(id: string, version: number, reason: string): Promise<Asset>;
  decideAsset(id: string, version: number, decision: Decision, reason?: string): Promise<Asset>;
}

const B = "/api/v1/resources";
export const PATHS = {
  suppliers: `${B}/institutions/{id}/suppliers`,
  supplierRegister: `${B}/suppliers/{id}/register`,
  supplierSubmit: `${B}/suppliers/{id}/submit`,
  supplierDecision: `${B}/suppliers/{id}/decision`,
  supplierSuspend: `${B}/suppliers/{id}/suspend`,
  supplierRestore: `${B}/suppliers/{id}/restore`,
  requisitions: `${B}/institutions/{id}/requisitions`,
  requisitionSubmit: `${B}/requisitions/{id}/submit`,
  requisitionDecision: `${B}/requisitions/{id}/decision`,
  requisitionCancel: `${B}/requisitions/{id}/cancel`,
  sourcingEvents: `${B}/institutions/{id}/sourcing-events`,
  sourcingEvent: `${B}/sourcing-events/{id}`,
  quotations: `${B}/sourcing-events/{id}/quotations`,
  evaluate: `${B}/sourcing-events/{id}/evaluate`,
  award: `${B}/sourcing-events/{id}/award`,
  purchaseOrders: `${B}/institutions/{id}/purchase-orders`,
  purchaseOrder: `${B}/purchase-orders/{id}`,
  orderIssue: `${B}/purchase-orders/{id}/issue`,
  orderDecision: `${B}/purchase-orders/{id}/decision`,
  orderAcknowledge: `${B}/purchase-orders/{id}/acknowledge`,
  orderCancel: `${B}/purchase-orders/{id}/cancel`,
  orderClose: `${B}/purchase-orders/{id}/close`,
  orderReceipts: `${B}/purchase-orders/{id}/receipts`,
  receipts: `${B}/institutions/{id}/goods-receipts`,
  receiptAccept: `${B}/goods-receipts/{id}/accept`,
  receiptReject: `${B}/goods-receipts/{id}/reject`,
  invoices: `${B}/institutions/{id}/vendor-invoices`,
  invoiceMatch: `${B}/vendor-invoices/{id}/match`,
  invoiceDecision: `${B}/vendor-invoices/{id}/decision`,
  invoicePost: `${B}/vendor-invoices/{id}/post`,
  payments: `${B}/institutions/{id}/vendor-payments`,
  paymentDecision: `${B}/vendor-payments/{id}/decision`,
  paymentPost: `${B}/vendor-payments/{id}/post`,
  items: `${B}/institutions/{id}/items`,
  itemActivate: `${B}/items/{id}/activate`,
  itemDeactivate: `${B}/items/{id}/deactivate`,
  stores: `${B}/institutions/{id}/stores`,
  stock: `${B}/institutions/{id}/stock-on-hand`,
  issues: `${B}/institutions/{id}/stock-issues`,
  issueDecision: `${B}/stock-issues/{id}/decision`,
  issueIssue: `${B}/stock-issues/{id}/issue`,
  issueReturn: `${B}/stock-issues/{id}/return`,
  reservations: `${B}/institutions/{id}/stock-reservations`,
  reservationRelease: `${B}/stock-reservations/{id}/release`,
  adjustments: `${B}/institutions/{id}/stock-adjustments`,
  adjustmentDecision: `${B}/stock-adjustments/{id}/decision`,
  replenishment: `${B}/institutions/{id}/replenishment`,
  assets: `${B}/institutions/{id}/assets`,
  assetAssign: `${B}/assets/{id}/assign`,
  assetReturn: `${B}/assets/{id}/return`,
  assetCondition: `${B}/assets/{id}/condition`,
  assetRetire: `${B}/assets/{id}/retire`,
  assetDecision: `${B}/assets/{id}/decision`,
} as const satisfies Record<string, keyof paths>;

const SUPPLIER_CMD: Record<SupplierCommand, string> = {
  register: PATHS.supplierRegister,
  submit: PATHS.supplierSubmit,
  suspend: PATHS.supplierSuspend,
  restore: PATHS.supplierRestore,
};

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

export function createHttpResourcesApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): ResourcesApi {
  async function request<T>(method: string, path: string, opts: RequestOptions = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = undefined;
      }
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  const id = (i: string) => ({ id: i });
  const reasonBody = (reason?: string) => (reason ? { reason } : {});
  const decide = <T>(template: string, i: string, v: number, decision: Decision, reason?: string) => request<T>("POST", fill(template, id(i)), { ifMatch: v, body: { decision, reason } });
  const create = <T>(template: string, i: string, body: unknown) => request<T>("POST", fill(template, id(i)), { body, idempotent: true });
  const command = <T>(template: string, i: string, v: number, body: unknown = {}) => request<T>("POST", fill(template, id(i)), { ifMatch: v, body });
  return {
    suppliers: (i) => items(fill(PATHS.suppliers, id(i))),
    createSupplier: (i, input) => create(PATHS.suppliers, i, input),
    supplierCommand: (i, v, cmd, reason) => command(SUPPLIER_CMD[cmd], i, v, reasonBody(reason)),
    decideSupplier: (i, v, d, reason) => decide(PATHS.supplierDecision, i, v, d, reason),
    requisitions: (i) => items(fill(PATHS.requisitions, id(i))),
    createRequisition: (i, input) => create(PATHS.requisitions, i, input),
    submitRequisition: (i, v) => command(PATHS.requisitionSubmit, i, v),
    decideRequisition: (i, v, d, reason) => decide(PATHS.requisitionDecision, i, v, d, reason),
    cancelRequisition: (i, v, reason) => command(PATHS.requisitionCancel, i, v, { reason }),
    sourcingEvents: (i) => items(fill(PATHS.sourcingEvents, id(i))),
    sourcingEvent: (i) => request("GET", fill(PATHS.sourcingEvent, id(i))),
    createSourcingEvent: (i, input) => create(PATHS.sourcingEvents, i, input),
    quote: (i, v, input) => command(PATHS.quotations, i, v, input),
    evaluate: (i, v) => command(PATHS.evaluate, i, v),
    award: (i, v, quotationId, reason) => command(PATHS.award, i, v, { quotationId, reason: reason || undefined }),
    purchaseOrders: (i) => items(fill(PATHS.purchaseOrders, id(i))),
    purchaseOrder: (i) => request("GET", fill(PATHS.purchaseOrder, id(i))),
    createPurchaseOrder: (i, input) => create(PATHS.purchaseOrders, i, input),
    issueOrder: (i, v) => command(PATHS.orderIssue, i, v),
    decideOrder: (i, v, d, reason) => decide(PATHS.orderDecision, i, v, d, reason),
    acknowledgeOrder: (i, v, reference) => command(PATHS.orderAcknowledge, i, v, { reference }),
    cancelOrder: (i, v, reason) => command(PATHS.orderCancel, i, v, { reason }),
    closeOrder: (i, v) => command(PATHS.orderClose, i, v),
    receipts: (i) => items(fill(PATHS.receipts, id(i))),
    receive: (po, input) => create(PATHS.orderReceipts, po, input),
    acceptReceipt: (i, v) => command(PATHS.receiptAccept, i, v),
    rejectReceipt: (i, v, reason) => command(PATHS.receiptReject, i, v, { reason }),
    invoices: (i) => items(fill(PATHS.invoices, id(i))),
    createInvoice: (i, input) => create(PATHS.invoices, i, input),
    matchInvoice: (i, v) => command(PATHS.invoiceMatch, i, v),
    decideInvoice: (i, v, d, reason) => decide(PATHS.invoiceDecision, i, v, d, reason),
    postInvoice: (i, v) => command(PATHS.invoicePost, i, v),
    payments: (i) => items(fill(PATHS.payments, id(i))),
    createPayment: (i, input) => create(PATHS.payments, i, input),
    decidePayment: (i, v, d, reason) => decide(PATHS.paymentDecision, i, v, d, reason),
    postPayment: (i, v) => command(PATHS.paymentPost, i, v),
    items: (i) => items(fill(PATHS.items, id(i))),
    createItem: (i, input) => create(PATHS.items, i, input),
    itemCommand: (i, v, cmd) => command(cmd === "activate" ? PATHS.itemActivate : PATHS.itemDeactivate, i, v),
    stores: (i) => items(fill(PATHS.stores, id(i))),
    createStore: (i, input) => create(PATHS.stores, i, input),
    stock: (i) => items(fill(PATHS.stock, id(i))),
    issues: (i) => items(fill(PATHS.issues, id(i))),
    createIssue: (i, input) => create(PATHS.issues, i, input),
    decideIssue: (i, v, d, reason) => decide(PATHS.issueDecision, i, v, d, reason),
    issueStock: (i, v) => command(PATHS.issueIssue, i, v),
    returnStock: (i, v, quantity, reason) => command(PATHS.issueReturn, i, v, { quantity, reason: reason || undefined }),
    reservations: (i) => items(fill(PATHS.reservations, id(i))),
    reserve: (i, input) => create(PATHS.reservations, i, input),
    releaseReservation: (i, v, reason) => command(PATHS.reservationRelease, i, v, reasonBody(reason)),
    adjustments: (i) => items(fill(PATHS.adjustments, id(i))),
    adjust: (i, input) => create(PATHS.adjustments, i, input),
    decideAdjustment: (i, v, d, reason) => decide(PATHS.adjustmentDecision, i, v, d, reason),
    replenishment: (i) => items(fill(PATHS.replenishment, id(i))),
    runReplenishment: (i, draftRequisition) => create(PATHS.replenishment, i, { draftRequisition }),
    assets: (i) => items(fill(PATHS.assets, id(i))),
    createAsset: (i, input) => create(PATHS.assets, i, input),
    assignAsset: (i, v, input) => command(PATHS.assetAssign, i, v, input),
    returnAsset: (i, v, condition) => command(PATHS.assetReturn, i, v, condition ? { condition } : {}),
    assetCondition: (i, v, input) => command(PATHS.assetCondition, i, v, input),
    retireAsset: (i, v, reason) => command(PATHS.assetRetire, i, v, { reason }),
    decideAsset: (i, v, d, reason) => decide(PATHS.assetDecision, i, v, d, reason),
  };
}
