/** In-memory `DemandApi` for mock mode and screen tests. */
import type { DemandApi } from "./api";
import { urgency } from "./demand";
import type { DemandRow, DemandSettings, ExpiryRow, ItemPlanning } from "./types";

export function sampleDemandRow(n: number, over: Partial<DemandRow> = {}): DemandRow {
  return {
    itemId: `item-${n}`, itemCode: `ITEM-${n}`, itemName: `Item ${n}`, unit: "each", onHand: "30.000", reserved: "0.000", available: "30.000",
    onOrder: "0.000", perDay: 0.5, rateLabel: "≈0.5 units a day", coverDays: 60, coverLabel: "60 days of cover", leadTimeDays: 2, safetyDays: 3,
    restockDays: 5, packSize: null, reorderLevel: "4.000", suggestedReorderLevel: "3", runningOut: false, status: "ok", suggestedOrderQuantity: "0", trackExpiry: false,
    ...over,
  };
}

export function createMockDemandApi(rows: DemandRow[] = [sampleDemandRow(1), sampleDemandRow(2, { status: "out", available: "0.000", coverDays: 0, coverLabel: "out of stock", suggestedOrderQuantity: "12" })], expiring: ExpiryRow[] = []): DemandApi {
  let settings: DemandSettings = { demandDays: 30, safetyDays: 3, coverageDays: 0, expiryDays: 30 };
  const planning = new Map<string, ItemPlanning>();
  return {
    demand: async (_id, o = {}) => {
      const items = rows.filter((r) => !o.attention || r.status !== "ok" || r.runningOut).sort((a, b) => urgency(a) - urgency(b));
      return { windowDays: o.days ?? settings.demandDays, measuredDays: 20, enough: true, items };
    },
    applyThresholds: async (_id, ids) => {
      const updatedIds: string[] = [];
      for (const r of rows) {
        if (ids.includes(r.itemId) && r.suggestedReorderLevel !== null && r.suggestedReorderLevel !== r.reorderLevel) {
          r.reorderLevel = r.suggestedReorderLevel;
          updatedIds.push(r.itemId);
        }
      }
      return { updatedIds, skipped: ids.length - updatedIds.length };
    },
    settings: async () => settings,
    saveSettings: async (_id, s) => {
      if (s.demandDays < 7) throw new Error("demand window must be 7 to 365 days");
      settings = s;
      return s;
    },
    savePlanning: async (id, p) => {
      planning.set(id, p);
      const r = rows.find((x) => x.itemId === id);
      if (r) {
        r.leadTimeDays = p.leadTimeDays;
        r.packSize = p.packSize;
        r.trackExpiry = p.trackExpiry;
      }
      return p;
    },
    lots: async (id) => ({ itemId: id, trackExpiry: planning.get(id)?.trackExpiry ?? false, horizonDays: settings.expiryDays, status: { expired: 0, soon: 0, soonest: "" }, items: [] }),
    expiry: async (_id, days) => ({ asOf: new Date().toISOString().slice(0, 10), horizonDays: days ?? settings.expiryDays, items: expiring }),
  };
}
