import { describe, expect, it } from "vitest";
import { brsLines, reconFigures } from "./bank-reconcile";
import type { BankReconItem } from "./types";

const item = (id: string, amount: string): BankReconItem => ({ journalEntryId: id, entryNumber: id, postingDate: "2026-05-01", description: "", amount, cleared: false });

// Mirrors Everything's tests/reconcile.test.js.
describe("bank reconciliation (ERP_EXTENSIONS 1B)", () => {
  const items = [item("dep", "1000.00"), item("e1", "-25.00")];
  it("computes the difference from the ticked items", () => {
    const f = reconFigures("0.00", items, new Set(["dep", "e1"]), "975.00");
    expect(f).toMatchObject({ beginning: "0.00", increases: "1000.00", decreases: "25.00", cleared: "975.00", difference: "0.00", balanced: true });
    const partial = reconFigures("0.00", items, new Set(["dep"]), "975.00");
    expect(partial.difference).toBe("-25.00");
    expect(partial.balanced).toBe(false);
  });
  it("carries the opening balance and treats a bad ending as zero", () => {
    const f = reconFigures("975.00", [item("e2", "-10.00")], new Set(["e2"]), "abc");
    expect(f.cleared).toBe("965.00");
    expect(f.difference).toBe("-965.00");
  });
  it("lays the BRS out in the textbook order", () => {
    const lines = brsLines({
      bankAccountId: "b", asOf: "2026-07-31", bookBalance: "100000.00", issuedNotPresented: "20000.00", depositedNotCredited: "5000.00",
      bankCreditsNotBooked: "300.00", bankDebitsNotBooked: "118.00", bankBalance: "115182.00", issued: [], deposited: [], bankCredits: [], bankDebits: [],
    });
    expect(lines.map((l) => l.amount)).toEqual(["100000.00", "20000.00", "5000.00", "300.00", "118.00", "115182.00"]);
    expect(lines[0]!.strong).toBe(true);
  });
});
