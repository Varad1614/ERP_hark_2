/** ERP_EXTENSIONS 2C: wire types of stock demand, item planning, lots and expiry. */
export interface DemandRow {
  itemId: string;
  itemCode: string;
  itemName: string;
  unit: string;
  onHand: string;
  reserved: string;
  available: string;
  onOrder: string;
  perDay: number;
  rateLabel: string;
  coverDays: number | null;
  coverLabel: string;
  leadTimeDays: number | null;
  safetyDays: number;
  restockDays: number;
  packSize: string | null;
  reorderLevel: string | null;
  suggestedReorderLevel: string | null;
  runningOut: boolean;
  status: "out" | "low" | "ok";
  suggestedOrderQuantity: string;
  trackExpiry: boolean;
}

export interface DemandReport {
  windowDays: number;
  measuredDays: number;
  enough: boolean;
  items: DemandRow[];
}

export interface DemandSettings {
  demandDays: number;
  safetyDays: number;
  coverageDays: number;
  expiryDays: number;
}

export interface ItemPlanning {
  leadTimeDays: number | null;
  packSize: string | null;
  safetyDays: number | null;
  trackExpiry: boolean;
}

export interface ExpiryStatus {
  expired: number;
  soon: number;
  soonest: string;
}

export interface LotRow {
  locationId: string;
  locationCode: string;
  expiryOn: string | null;
  quantity: string;
}

export interface LotList {
  itemId: string;
  trackExpiry: boolean;
  horizonDays: number;
  status: ExpiryStatus;
  items: LotRow[];
}

export interface ExpiryRow {
  itemId: string;
  itemCode: string;
  itemName: string;
  unit: string;
  expired: string;
  soon: string;
  soonest: string | null;
  value: string;
}

export interface ExpiryReport {
  asOf: string;
  horizonDays: number;
  items: ExpiryRow[];
}
