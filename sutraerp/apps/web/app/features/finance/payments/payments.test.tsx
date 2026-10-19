/** Slice 5B: paise arithmetic without floats and the mock gateway / refund flow of the in-memory API. */
import { describe, expect, it } from "vitest";
import { PATHS } from "./api";
import { createMockPaymentsApi, fromPaise, toPaise } from "./mock";

describe("5B money helpers", () => {
  it("round-trips decimal strings through integer paise", () => {
    expect(toPaise("125000.00")).toBe(12500000n);
    expect(toPaise("0.5")).toBe(50n);
    expect(toPaise("-12.34")).toBe(-1234n);
    expect(fromPaise(12500000n)).toBe("125000.00");
    expect(fromPaise(5n)).toBe("0.05");
    expect(fromPaise(-1234n)).toBe("-12.34");
  });

  it("keeps every path under the finance prefix", () => {
    for (const p of Object.values(PATHS)) expect(p.startsWith("/api/v1/finance/")).toBe(true);
  });
});

describe("5B mock API", () => {
  it("pays online, issues a receipt and refunds credit", async () => {
    const api = createMockPaymentsApi();
    await expect(api.initiate("125000.01")).rejects.toBeTruthy();
    const failed = await api.confirm((await api.initiate("10.13")).id, 1);
    expect(failed.status).toBe("failed");
    const pending = await api.confirm((await api.initiate("10.99")).id, 1);
    expect(pending.status).toBe("pending");
    const paid = await api.confirm((await api.initiate("25000.00")).id, 1);
    expect(paid.status).toBe("completed");
    expect(paid.receipt?.receiptNumber).toBe("RCT-000001");
    expect((await api.myAccounts())[0]!.account.balance.outstanding).toBe("100000.00");
    const counter = await api.recordPayment("acc", { channel: "cash", amount: "100500.00" });
    expect(counter.unappliedAmount).toBe("500.00");
    await expect(api.requestMyRefund({ amount: "500.01", reason: "x" })).rejects.toBeTruthy();
    const r = await api.requestMyRefund({ amount: "500.00", reason: "overpaid" });
    expect(r.status).toBe("approved");
    expect(r.history.map((h) => h.toStatus)).toEqual(["requested", "under_review", "approved"]);
    const done = await api.refundCommand((await api.refundCommand(r.id, r.version, "process")).id, r.version, "complete");
    expect(done.status).toBe("completed");
  });
});
