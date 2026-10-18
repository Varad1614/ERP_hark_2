import { describe, expect, it } from "vitest";
import {
  allowedActions, annualCtc, canEdit, ctcTotals, employerPfOf, fromPaise, grossMonthly, isTerminal, monthlyCtc, offerDates,
  offerStatusLabel, offerSummary, suggestedEmployerPf, toPaise,
} from "./offers";

describe("paise arithmetic", () => {
  it("parses and prints decimal strings exactly", () => {
    expect(toPaise("1800.50")).toBe(180050);
    expect(toPaise("1,25,000")).toBe(12500000);
    expect(toPaise(0.1)).toBe(10);
    expect(toPaise("")).toBe(0);
    expect(toPaise(null)).toBe(0);
    expect(toPaise("abc")).toBe(0);
    expect(fromPaise(180050)).toBe("1800.50");
    expect(fromPaise(-5)).toBe("-0.05");
  });
});

describe("employer PF", () => {
  it("is 12 % of basic + DA, capped at the ₹15,000 wage ceiling", () => {
    expect(suggestedEmployerPf("10000", "2000")).toBe("1440.00");
    expect(suggestedEmployerPf("45000", "5000")).toBe("1800.00");
    expect(suggestedEmployerPf("15000", "0")).toBe("1800.00");
    expect(suggestedEmployerPf("0", "")).toBe("0.00");
  });
  it("rounds to whole rupees half-to-even like the server", () => {
    expect(suggestedEmployerPf("12512.50", "0")).toBe("1502.00"); // 1501.50 → 1502
    expect(suggestedEmployerPf("12504.17", "0")).toBe("1501.00"); // 1500.5004 → 1501
    expect(suggestedEmployerPf("12487.50", "0")).toBe("1498.00"); // 1498.50 → 1498
  });
  it("uses the typed PF and falls back to the suggestion when blank", () => {
    expect(employerPfOf({ basic: "20000", employerPf: "1000" })).toBe("1000.00");
    expect(employerPfOf({ basic: "20000", employerPf: "" })).toBe("1800.00");
    expect(employerPfOf({ basic: "10000", da: "1000" })).toBe("1320.00");
  });
});

describe("CTC totals", () => {
  const ctc = { basic: "45000", hra: "18000", da: "5000", allowances: "7000.50", employerPf: "1800" };
  it("adds the breakup per month and ×12 per year", () => {
    expect(grossMonthly(ctc)).toBe("75000.50");
    expect(monthlyCtc(ctc)).toBe("76800.50");
    expect(annualCtc(ctc)).toBe("921606.00");
  });
  it("formats the totals in ₹ with Indian grouping", () => {
    expect(ctcTotals(ctc)).toEqual({ gross: "₹75,000.50", pf: "₹1,800.00", monthly: "₹76,800.50", annual: "₹9,21,606.00" });
  });
  it("counts the suggested PF when none is typed", () => {
    expect(monthlyCtc({ basic: 30000, hra: 12000 })).toBe("43800.00");
    expect(annualCtc({ basic: 30000, hra: 12000 })).toBe("525600.00");
  });
});

describe("status", () => {
  it("labels every status", () => {
    expect(offerStatusLabel("pending_approval")).toBe("Awaiting approval");
    expect(offerStatusLabel("mystery")).toBe("mystery");
  });
  it("allows the right actions per status", () => {
    expect(allowedActions("draft")).toEqual(["submit", "withdraw"]);
    expect(allowedActions("pending_approval")).toEqual(["approve", "reject"]);
    expect(allowedActions("approved")).toEqual(["send", "withdraw"]);
    expect(allowedActions("sent")).toEqual(["accept", "decline", "withdraw"]);
    for (const s of ["accepted", "declined", "expired", "withdrawn", "mystery"]) {
      expect(allowedActions(s)).toEqual([]);
      expect(isTerminal(s)).toBe(true);
    }
    expect(isTerminal("sent")).toBe(false);
    expect(canEdit("draft")).toBe(true);
    expect(canEdit("approved")).toBe(false);
  });
});

describe("formatting", () => {
  it("summarises an offer and its dates", () => {
    expect(offerSummary({ number: "OFR-000012", designation: "Assistant Professor", ctcAnnual: "600000" })).toBe("OFR-000012 · Assistant Professor · ₹6,00,000.00 a year");
    expect(offerDates({ joiningDate: "2026-06-01", expiresOn: "2026-05-20" })).toBe("Joins 1 Jun 2026 · expires 20 May 2026");
    expect(offerDates({ joiningDate: "2026-06-01", expiresOn: null })).toBe("Joins 1 Jun 2026");
  });
});
