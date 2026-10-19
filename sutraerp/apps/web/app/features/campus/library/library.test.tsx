/** Slice 8C web (library): mock API rules, HTTP request shape, action rules, the Admin → Campus → Library tab and the Student library panel. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../../institution/types";
import { createHttpLibraryApi, PATHS } from "./api";
import { createMockLibraryApi } from "./mock";
import { copyActions, fineActions, memberActions, policyActions, reservationActions, sumAmounts } from "./index";

const INST = { id: "inst-8c", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("8C library API (mock)", () => {
  it("drafts, approves and activates a borrowing policy, superseding the active one", async () => {
    const api = createMockLibraryApi();
    let p = await api.createPolicy("i", { name: "Rules 2027", effectiveFrom: "2027-06-01", loanDaysStudent: 14, loanDaysStaff: 30, maxLoansStudent: 3, maxLoansStaff: 6, maxRenewals: 1, finePerDay: "2.00", reservationHoldDays: 3, lostChargeFactor: "1.5" });
    expect(policyActions(p)).toEqual(["submit"]);
    p = await api.policyCommand(p.id, p.version, "submit");
    expect(policyActions(p)).toEqual(["approve", "reject"]);
    p = await api.decidePolicy(p.id, p.version, "approve");
    expect(policyActions(p)).toEqual(["activate"]);
    p = await api.policyCommand(p.id, p.version, "activate");
    const all = await api.policies("i");
    expect(all.filter((x) => x.status === "active").map((x) => x.id)).toEqual([p.id]);
  });

  it("issues an available copy only, renews once and returns", async () => {
    const api = createMockLibraryApi();
    const [member] = await api.memberships("i");
    await expect(api.issue("i", { membershipId: member!.id, accessionNumber: "ACC-000001" })).rejects.toThrow();
    let loan = await api.issue("i", { membershipId: member!.id, accessionNumber: "ACC-000002" });
    expect(loan.status).toBe("active");
    loan = await api.loanCommand(loan.id, loan.version, "renew");
    await expect(api.loanCommand(loan.id, loan.version, "renew")).rejects.toThrow();
    loan = await api.loanCommand(loan.id, loan.version, "return");
    expect(loan.status).toBe("returned");
  });

  it("bills a student's fine once and refuses a waiver after billing", async () => {
    const api = createMockLibraryApi();
    const [fine] = await api.fines("i", "assessed");
    expect(fineActions(fine!)).toEqual(["bill", "waive"]);
    const billed = await api.fineCommand(fine!.id, fine!.version, "bill");
    expect(billed.status).toBe("billed");
    expect(billed.chargeNumber).toBeTruthy();
    await expect(api.fineCommand(billed.id, billed.version, "waiver", "Hardship")).rejects.toThrow();
  });

  it("only requests a waiver; it is waived once another user decides it (DEF-G-002)", async () => {
    const api = createMockLibraryApi();
    const [fine] = await api.fines("i", "assessed");
    const requested = await api.fineCommand(fine!.id, fine!.version, "waiver", "Medical leave");
    expect(requested.status).toBe("waiver_requested");
    expect(fineActions(requested)).toEqual(["approve", "reject"]);
    const waived = await api.decideFine(requested.id, requested.version, "approve");
    expect(waived.status).toBe("waived");
  });

  it("marks an issued copy lost with a replacement charge, and blocks clearance while loans are out", async () => {
    const api = createMockLibraryApi();
    const [record] = await api.catalogue("i");
    const issued = (await api.copies(record!.id)).find((c) => c.status === "issued")!;
    expect(copyActions(issued)).toEqual(["condition", "lost"]);
    const lost = await api.copyCommand(issued.id, issued.version, "lost", "Not returned");
    expect(lost.status).toBe("lost");
    const replacement = (await api.fines("i")).find((f) => f.kind === "replacement");
    expect(replacement?.amount).toBe("675.00");
    const [member] = await api.memberships("i");
    const c = await api.requestClearance("i", member!.id, "Programme completion");
    expect(c.status).toBe("rejected");
    expect(c.verification.blockers.length).toBeGreaterThan(0);
    const [, staff] = await api.memberships("i");
    expect((await api.requestClearance("i", staff!.id, "Transfer")).status).toBe("approved");
  });

  it("serves the desk reservation queue and the student's own reservations", async () => {
    const api = createMockLibraryApi();
    const [queued] = await api.reservations("i", "queued");
    expect(reservationActions(queued!)).toEqual(["cancel"]);
    await expect(api.reservationCommand(queued!.id, queued!.version, "collect")).rejects.toThrow();
    const records = await api.myCatalogue();
    const out = records.find((r) => r.availableCopies === 0)!;
    const mine = await api.reserve(out.id);
    expect(mine.queuePosition).toBe(2);
    await expect(api.reserve(out.id)).rejects.toThrow();
    const cancelled = await api.cancelMine(mine.id, mine.version);
    expect(cancelled.status).toBe("cancelled");
    const me = await api.mine();
    expect(me.membership?.number).toBe("LM-000001");
    expect(me.eligibility?.unsettledFines).toBe("12.00");
  });
});

describe("8C library action rules", () => {
  it("offers the next legal steps", () => {
    expect(memberActions({ status: "active", restrictionReason: null })).toEqual(["restrict", "suspend", "close"]);
    expect(memberActions({ status: "active", restrictionReason: "Unpaid fines" })).toEqual(["lift", "suspend", "close"]);
    expect(memberActions({ status: "suspended" })).toEqual(["restore", "close"]);
    expect(memberActions({ status: "closed" })).toEqual([]);
    expect(fineActions({ status: "assessed", personKind: "staff" })).toEqual(["waive"]);
    expect(fineActions({ status: "waiver_requested", personKind: "student" })).toEqual(["approve", "reject"]);
    expect(fineActions({ status: "billed", personKind: "student" })).toEqual([]);
    expect(reservationActions({ status: "ready" })).toEqual(["collect", "expire", "cancel"]);
    expect(copyActions({ status: "withdrawn" })).toEqual([]);
    expect(policyActions({ status: "active", approvalState: "approved" })).toEqual([]);
  });

  it("sums money without float drift", () => {
    expect(sumAmounts(["0.10", "0.20"])).toBe("0.30");
    expect(sumAmounts(["12.00", "2.5", "100"])).toBe("114.50");
    expect(sumAmounts([])).toBe("0.00");
  });
});

describe("8C library HTTP API", () => {
  it("sends If-Match on commands, Idempotency-Key on creates and the documented paths", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "x1", status: "active", version: 3, items: [] }), { status: 200 });
    };
    const api = createHttpLibraryApi(fetchImpl, "");
    await api.restrict("m1", 2, true, "Unpaid fines");
    await api.requestClearance("inst", "m1", "Transfer");
    await api.reservationCommand("r1", 4, "collect");
    await api.loans("inst", "overdue");
    const h0 = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.restriction.replace("{id}", "m1"));
    expect(h0["If-Match"]).toBe('"2"');
    expect(JSON.parse(String(calls[0]?.init.body))).toEqual({ restricted: true, reason: "Unpaid fines" });
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    expect(JSON.parse(String(calls[1]?.init.body))).toEqual({ membershipId: "m1", purpose: "Transfer" });
    expect(calls[2]?.url).toBe("/api/v1/campus/library-reservations/r1/collect");
    expect(calls[3]?.url).toBe("/api/v1/campus/institutions/inst/library-loans?status=overdue&limit=200");
  });
});

describe("8C library screens", () => {
  it("renders the Admin → Campus → Library tab", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { LibraryAdminTab } = await import("./index");
    await renderScreen(<LibraryAdminTab institution={INST} />);
    expect(document.querySelector("[data-slice='8C']")).toBeTruthy();
    expect((await screen.findAllByText("LM-000001")).length).toBeGreaterThan(0);
    expect((await screen.findAllByText("Library rules 2026")).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Check clearance" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Issue" })).toBeTruthy();
  });

  it("renders the student's library panel with loans, search and clearance", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { MyLibraryPanel } = await import("./index");
    await renderScreen(<MyLibraryPanel />, { role: "student" });
    expect((await screen.findAllByText("Engineering Thermodynamics")).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Request clearance" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Report a problem" })).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { LibraryAdminTab } = await import("./index");
    await renderScreen(<LibraryAdminTab institution={null} />);
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
