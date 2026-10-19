/**
 * Batch 5 (Domain F, Finance) web feature — tab and screen definitions (foundation-owned,
 * docs/batch5-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the routes:
 * `charges/` (5A), `payments/` (5B), `ledger/` (5C). Route modules stay thin.
 */
export { activeTab, type TabDef } from "../institution";

/**
 * Tabs of Admin → Finance (`/finance/:tab?`, first = default) with their owning slice. Re-skin v2:
 * the area header shows Overview / Collections / Receivables / Ledger and folds the rest into
 * **More** (`AreaHeader` keeps ≤5 header tabs), so the order here is the header order.
 */
export const FINANCE_TABS = [
  { key: "overview", label: "Overview", slice: "R5" },
  { key: "collections", label: "Collections", slice: "5B" },
  { key: "receivables", label: "Receivables", slice: "5B" },
  { key: "ledger", label: "Ledger", slice: "5C" },
  { key: "charges", label: "Fees & charges", slice: "5A" },
  { key: "adjustments", label: "Adjustments", slice: "5A" },
  { key: "aid", label: "Financial aid", slice: "5A" },
  { key: "refunds", label: "Refunds", slice: "5B" },
  { key: "periods", label: "Periods", slice: "5C" },
  { key: "treasury", label: "Bank & treasury", slice: "5C" },
] as const;

/** Role-facing Batch 5 screens: route path → the slices that render panels on it and the roles that may open it. */
export const FINANCE_SCREENS = {
  finance: { slices: ["5A", "5B", "5C"], roles: ["administrator"] },
  fees: { slices: ["5A", "5B"], roles: ["student"] },
} as const;

/** Path of an Admin → Finance tab. */
export function financePath(tab?: string): string {
  return `/finance${tab ? `/${tab}` : ""}`;
}

/**
 * Legacy Batch 5 rupee display (`"125000.00"` → `₹1,25,000.00`, always two decimals), kept only
 * for the other areas that still import it (campus, home, reports — their re-skin slices move them
 * to `formatINR`). **Finance screens no longer use it**: they format through the R0 `formatINR`
 * (`money` / `ledger` / `compact` in `./ui`), i.e. no needless `.00`, paise kept on ledgers.
 */
export function formatInr(amount: string | null | undefined): string {
  if (amount == null || amount === "") return "—";
  const negative = amount.startsWith("-");
  const [intRaw = "0", fracRaw = ""] = amount.replace(/^[-+]/, "").split(".");
  const frac = (fracRaw + "00").slice(0, 2);
  const int = intRaw.replace(/^0+(?=\d)/, "");
  const last3 = int.slice(-3);
  const rest = int.slice(0, -3).replace(/\B(?=(\d{2})+(?!\d))/g, ",");
  return `${negative ? "-" : ""}₹${rest ? `${rest},` : ""}${last3}.${frac}`;
}
