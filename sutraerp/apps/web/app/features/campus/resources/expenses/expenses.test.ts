import { describe, expect, it } from "vitest";
import { base64Bytes, fitWithin, flagLabel, formFromReading, formReady, inputOfForm, inr, isBlocking, lowConfidence, monthLabel, ocrMismatch, submitBlockers } from "./expenses";
import { createMockExpensesApi, MOCK_READING, MOCK_SETTINGS } from "./mock";
import { readingNote } from "./MyExpensesTile";

const TODAY = "2026-10-17";

describe("expense helpers", () => {
  it("fits photos within 1600 px on the long edge without upscaling", () => {
    expect(fitWithin(4000, 3000)).toEqual({ width: 1600, height: 1200 });
    expect(fitWithin(3024, 4032)).toEqual({ width: 1200, height: 1600 });
    expect(fitWithin(1200, 900)).toEqual({ width: 1200, height: 900 });
    expect(fitWithin(10000, 10)).toEqual({ width: 1600, height: 2 });
    expect(base64Bytes("QUJD")).toBe(3);
    expect(base64Bytes("QUI=")).toBe(2);
    expect(base64Bytes("QQ==")).toBe(1);
  });

  it("words flags plainly and knows which block a submission", () => {
    expect(flagLabel("over_limit")).toBe("Over the category limit");
    expect(flagLabel("receipt_missing")).toBe("Receipt needed");
    expect(flagLabel("too_old", 90)).toBe("Older than 90 days");
    expect(flagLabel("foreign_currency")).toBe("Only rupee expenses can be claimed");
    expect(["receipt_missing", "too_old", "future_date", "foreign_currency"].every(isBlocking)).toBe(true);
    expect(isBlocking("over_limit") || isBlocking("uncategorised")).toBe(false);
    expect(submitBlockers({ canSubmit: false, lineCount: 2, flags: ["over_limit", "too_old"], status: "draft" }, 90)).toEqual(["Fix: older than 90 days."]);
    expect(submitBlockers({ canSubmit: false, lineCount: 0, flags: [], status: "draft" })).toEqual(["Add at least one expense."]);
    expect(submitBlockers({ canSubmit: true, lineCount: 1, flags: [], status: "draft" })).toEqual([]);
  });

  it("formats money with lakh grouping", () => {
    expect(inr("1234567.5")).toBe("₹12,34,567.50");
    expect(inr("378.00")).toBe("₹378.00");
    expect(inr(100000)).toBe("₹1,00,000.00");
    expect(inr(null)).toBe("—");
    expect(monthLabel("2026-10")).toBe("Oct 26");
    expect(ocrMismatch({ amount: "525.00", ocr: { total: 552 } })).toEqual({ read: 552, claimed: 525 });
    expect(ocrMismatch({ amount: "640.00", ocr: { total: 640 } })).toBeNull();
    expect(ocrMismatch({ amount: "640.00", ocr: null })).toBeNull();
  });

  it("pre-fills the confirm form from what the receipt read", () => {
    const f = formFromReading({ status: "read", engine: "paddleocr", fields: { ...MOCK_READING, date: "2026-10-15" }, gstin: "27AAPFU0939F1ZV", lineCount: 12 }, "rc-9", TODAY, MOCK_SETTINGS);
    expect(f).toMatchObject({ spentOn: "2026-10-15", merchant: "Hotel Sai Palace, Ahilyanagar", amount: "378.00", tax: "18.00", categoryId: "meals", paymentMethod: "Mobile payment", gstin: "27AAPFU0939F1ZV", receiptId: "rc-9", currency: "INR" });
    expect(formReady(f, TODAY)).toBe(true);
    expect(inputOfForm({ ...f, notes: "  " })).toMatchObject({ amount: "378.00", notes: null, categoryId: "meals", receiptId: "rc-9" });
    const odd = formFromReading({ status: "read", engine: "paddleocr", fields: { ...MOCK_READING, date: "2027-01-01", categoryId: "fuel", paymentMethod: "VISA", total: null, confidence: 0.3 }, lineCount: 3 }, "rc-8", TODAY, MOCK_SETTINGS);
    expect(odd).toMatchObject({ spentOn: TODAY, categoryId: "", paymentMethod: "", amount: "" });
    expect(formReady(odd, TODAY)).toBe(false);
    expect(lowConfidence({ status: "read", engine: "paddleocr", fields: { ...MOCK_READING, confidence: 0.3 }, lineCount: 3 })).toBe(true);
    const empty = formFromReading({ status: "disabled", engine: "none", lineCount: 0 }, "rc-7", TODAY);
    expect(empty).toMatchObject({ merchant: "", amount: "", spentOn: TODAY, receiptId: "rc-7" });
    expect(readingNote({ status: "disabled", engine: "none", lineCount: 0 }, "none").text).toMatch(/type the details in/);
    expect(readingNote({ status: "read", engine: "paddleocr", fields: MOCK_READING, lineCount: 9 }, "paddleocr").text).toBe("Read by PaddleOCR — check every field.");
  });
});

describe("mock expenses api", () => {
  it("runs a claim from receipt to reimbursement", async () => {
    const api = createMockExpensesApi(TODAY);
    const rc = await api.uploadReceipt({ fileName: "bill.jpg", contentType: "image/jpeg", dataBase64: "QUJD" });
    expect(rc.reading.status).toBe("read");
    const form = formFromReading(rc.reading, rc.id, TODAY, MOCK_SETTINGS);
    const e = await api.createExpense(inputOfForm(form));
    expect(e.flags).toEqual([]);
    const me = await api.mine();
    const old = me.unclaimed.find((x) => x.merchant.startsWith("MSRTC"));
    expect(old?.flags).toContain("too_old");
    let claim = await api.createClaim({ expenseIds: [e.id, old?.id ?? ""] });
    expect(claim.status).toBe("draft");
    expect(claim.canSubmit).toBe(false);
    claim = await api.updateClaim(claim.id, { expenseIds: [e.id], version: claim.version });
    expect(claim.canSubmit).toBe(true);
    claim = await api.submit(claim.id, claim.version);
    await expect(api.decide(claim.id, { decision: "return", version: claim.version })).rejects.toThrow(/why/);
    claim = await api.decide(claim.id, { decision: "return", reason: "Attach the GST bill", version: claim.version });
    expect(claim.status).toBe("draft");
    expect(claim.returnedReason).toBe("Attach the GST bill");
    claim = await api.submit(claim.id, claim.version);
    expect(claim.returnedReason).toBeNull();
    claim = await api.decide(claim.id, { decision: "approve", version: claim.version });
    claim = await api.reimburse(claim.id, { paidOn: TODAY, reference: "UTR123", version: claim.version });
    expect(claim.status).toBe("reimbursed");
    const dash = await api.dashboard("i");
    expect(dash.awaitingApproval).toBe(1);
    expect(dash.ocrMismatches).toBe(1);
    expect(dash.trend).toHaveLength(6);
  });
});
