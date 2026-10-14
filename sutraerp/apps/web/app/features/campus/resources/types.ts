/**
 * Slice-8D view types (procurement, inventory, assets, payables). They mirror the API schemas
 * (`SupplierView`, `PurchaseRequisitionDetail`, `PurchaseOrderDetail`, …); money and quantities
 * stay decimal **strings** end to end (never parsed into floats for display).
 */

export interface Items<T> {
  items: T[];
}

export type Decision = "approve" | "reject";

export interface Supplier {
  id: string;
  institutionId: string;
  number: string;
  legalName: string;
  tradeName?: string | null;
  gstin?: string | null;
  pan?: string | null;
  email?: string | null;
  phone?: string | null;
  category?: string | null;
  /** Masked on reads (`XXXXXX7890`). */
  bankAccountNumber?: string | null;
  bankIfsc?: string | null;
  status: string;
  statusReason?: string | null;
  approvalPending: boolean;
  canonicalSupplierId?: string | null;
  version: number;
}

export interface SupplierInput {
  legalName: string;
  gstin?: string;
  pan?: string;
  email?: string;
  bankAccountName?: string;
  bankAccountNumber?: string;
  bankIfsc?: string;
}

export type SupplierCommand = "register" | "submit" | "suspend" | "restore";

export interface RequisitionLine {
  lineNo: number;
  itemId?: string | null;
  itemCode?: string | null;
  description: string;
  quantity: string;
  unit: string;
  estimatedUnitCost: string;
  estimatedAmount: string;
}

export interface Requisition {
  id: string;
  institutionId: string;
  number: string;
  title: string;
  justification?: string | null;
  revision: number;
  estimatedTotal: string;
  status: string;
  statusReason?: string | null;
  source: string;
  neededBy?: string | null;
  version: number;
  lines?: RequisitionLine[];
}

export interface RequisitionLineInput {
  itemId?: string;
  description?: string;
  quantity: string;
  estimatedUnitCost?: string;
}

export interface Quotation {
  id: string;
  supplierId: string;
  supplierNumber: string;
  supplierName: string;
  totalAmount: string;
  rank?: number | null;
  status: string;
  lines?: Array<{ lineNo: number; unitPrice: string; amount: string }>;
}

export interface SourcingEvent {
  id: string;
  number: string;
  requisitionId: string;
  requisitionNumber: string;
  method: string;
  title: string;
  estimatedValue: string;
  status: string;
  pendingQuotationId?: string | null;
  awardedQuotationId?: string | null;
  awardedSupplierId?: string | null;
  version: number;
  lines?: RequisitionLine[];
  quotations?: Quotation[];
}

export interface PurchaseOrderLine {
  id: string;
  lineNo: number;
  itemId?: string | null;
  itemCode?: string | null;
  lineKind: string;
  description: string;
  quantity: string;
  unit: string;
  unitPrice: string;
  amount: string;
  receivedQuantity: string;
  invoicedQuantity: string;
}

export interface PurchaseOrder {
  id: string;
  institutionId: string;
  number: string;
  supplierId: string;
  supplierNumber: string;
  supplierName: string;
  requisitionId?: string | null;
  sourcingEventId?: string | null;
  deliveryLocationId?: string | null;
  orderDate: string;
  revision: number;
  totalAmount: string;
  status: string;
  approvalPending: boolean;
  acknowledgementRef?: string | null;
  version: number;
  lines?: PurchaseOrderLine[];
}

export interface GoodsReceiptLine {
  id: string;
  lineNo: number;
  purchaseOrderLineNo: number;
  lineKind: string;
  description: string;
  receivedQuantity: string;
  acceptedQuantity: string;
  rejectedQuantity: string;
  assetsRegistered: number;
}

export interface GoodsReceipt {
  id: string;
  number: string;
  purchaseOrderId: string;
  purchaseOrderNumber: string;
  supplierNumber: string;
  locationCode: string;
  receivedOn: string;
  status: string;
  version: number;
  lines?: GoodsReceiptLine[];
}

export interface InvoiceFinding {
  lineNo: number;
  code: string;
  message: string;
}

export interface VendorInvoice {
  id: string;
  number: string;
  supplierId: string;
  supplierNumber: string;
  supplierName: string;
  purchaseOrderId: string;
  purchaseOrderNumber: string;
  supplierInvoiceNumber: string;
  isCreditNote: boolean;
  invoiceDate: string;
  totalAmount: string;
  paidAmount: string;
  status: string;
  matchFindings: InvoiceFinding[];
  approvalPending: boolean;
  entryNumber?: string | null;
  version: number;
}

export interface VendorPayment {
  id: string;
  number: string;
  supplierId: string;
  supplierNumber: string;
  paymentDate: string;
  amount: string;
  status: string;
  entryNumber?: string | null;
  version: number;
}

export interface Item {
  id: string;
  code: string;
  name: string;
  kind: string;
  unit: string;
  reorderLevel?: string | null;
  standardCost?: string | null;
  status: string;
  onHand: string;
  reserved: string;
  version: number;
}

export interface ItemInput {
  code: string;
  name: string;
  kind: string;
  unit?: string;
  reorderLevel?: string;
  standardCost?: string;
}

export interface StoreLocation {
  id: string;
  parentId?: string | null;
  code: string;
  name: string;
  status: string;
  version: number;
}

export interface StockLine {
  itemId: string;
  itemCode: string;
  itemName: string;
  locationId: string;
  locationCode: string;
  onHand: string;
  unit: string;
  reorderLevel?: string | null;
  atReorderLevel: boolean;
}

export interface StockIssue {
  id: string;
  number: string;
  itemId: string;
  itemCode: string;
  locationCode: string;
  quantity: string;
  returnedQuantity: string;
  purpose: string;
  status: string;
  version: number;
}

export interface Reservation {
  id: string;
  itemCode: string;
  locationCode: string;
  quantity: string;
  purpose: string;
  status: string;
  version: number;
}

export interface Adjustment {
  id: string;
  itemCode: string;
  locationCode: string;
  quantity: string;
  reason: string;
  status: string;
  version: number;
}

export interface ReplenishmentNeed {
  id: string;
  itemCode: string;
  itemName: string;
  onHand: string;
  reorderLevel: string;
  suggestedQuantity: string;
  status: string;
  requisitionId?: string | null;
}

export interface BulkResult {
  processed: number;
  skipped: number;
  createdIds: string[];
}

export interface Asset {
  id: string;
  number: string;
  name: string;
  serialNumber?: string | null;
  acquisitionCost?: string | null;
  acquiredOn?: string | null;
  condition: string;
  status: string;
  custodianKind?: string | null;
  custodianId?: string | null;
  retirementPending: boolean;
  version: number;
}

export interface AssetInput {
  name: string;
  serialNumber?: string;
  acquisitionCost?: string;
  goodsReceiptLineId?: string;
}
