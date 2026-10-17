/**
 * ERP_EXTENSIONS 2C screen helpers. The arithmetic itself runs on the server (golden-tested
 * against Everything's inventory demand.js / lots.js); these only word and order it.
 */
import type { DemandRow, DemandSettings } from "./types";

/** Out of stock first, then fewest days of cover, unknown demand last. */
export function urgency(r: Pick<DemandRow, "status" | "coverDays">): number {
  return r.status === "out" ? -1 : (r.coverDays ?? Number.POSITIVE_INFINITY);
}

/** What the row asks of the store keeper, in a few words. */
export function attention(r: DemandRow): string {
  if (r.status === "out") return "Out of stock";
  if (r.runningOut) return `Runs out before a restock (${r.restockDays} days)`;
  if (r.status === "low") return "At or below reorder level";
  return "";
}

/** A suggested reorder level that differs from the one set. */
export function differs(r: Pick<DemandRow, "reorderLevel" | "suggestedReorderLevel">): boolean {
  if (r.suggestedReorderLevel === null) return false;
  return r.reorderLevel === null || Number(r.reorderLevel) !== Number(r.suggestedReorderLevel);
}

const qty = (s: string | null) => (s === null ? "—" : String(Number(s)));
export { qty };

/** Settings form check: the server's ranges. */
export function settingsProblem(s: DemandSettings): string | null {
  if (!(s.demandDays >= 7 && s.demandDays <= 365)) return "Demand window must be 7 to 365 days";
  if (!(s.safetyDays >= 0 && s.safetyDays <= 365)) return "Safety days must be 0 to 365";
  if (!(s.coverageDays >= 0 && s.coverageDays <= 365)) return "Coverage must be 0 to 365 days";
  if (!(s.expiryDays >= 1 && s.expiryDays <= 365)) return "Expiry horizon must be 1 to 365 days";
  return null;
}
