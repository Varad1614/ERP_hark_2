/** Slice 7B web: mock API rules, HTTP request shape and the faculty My leave panel. */
import { screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHttpLeaveApi, PATHS } from "./api";
import { createMockLeaveApi, fromHundredths, leaveDays, toHundredths } from "./mock";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("7B leave API", () => {
  it("counts working days in hundredths, never floats", () => {
    expect(toHundredths("2.50")).toBe(250);
    expect(fromHundredths(-150)).toBe("-1.50");
    // Thu 30 Jul → Tue 4 Aug 2026, Sunday off, half last day.
    expect(fromHundredths(leaveDays("2026-07-30", "2026-08-04", false, true))).toBe("4.50");
  });

  it("reserves on approval, consumes once and refuses an overdraft (mock)", async () => {
    const api = createMockLeaveApi();
    const types = await api.myTypes();
    const cl = types.find((t) => t.code === "CL")!;
    const req = await api.apply({ leaveTypeId: cl.id, startDate: "2026-09-01", endDate: "2026-09-02" });
    expect(req.status).toBe("reserved");
    expect(req.days).toBe("2.00");
    const balance = (await api.myBalances()).find((b) => b.leaveTypeCode === "CL")!;
    expect(balance.reserved).toBe("2.00");
    expect(balance.available).toBe("10.00");
    const consumed = await api.consumeRequest(req.id, req.version);
    expect(consumed.status).toBe("consumed");
    await expect(api.consumeRequest(req.id, consumed.version)).rejects.toThrow();
    await expect(api.apply({ leaveTypeId: cl.id, startDate: "2026-10-01", endDate: "2026-10-31" })).rejects.toThrow();
  });

  it("sends If-Match on decisions and Idempotency-Key on creates", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "r1", status: "reserved", version: 3 }), { status: 200 });
    };
    const api = createHttpLeaveApi(fetchImpl, "");
    await api.decideApproval("r1", 2, true);
    const headers = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.approvalDecision.replace("{id}", "r1"));
    expect(headers["If-Match"]).toBe('"2"');
    expect(JSON.parse(String(calls[0]?.init.body)).decision).toBe("approve");
    await api.apply({ leaveTypeId: "t", startDate: "2026-09-01", endDate: "2026-09-01" });
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
  });
});

describe("Faculty My leave panel", () => {
  it("shows the inline apply form with leave type names (no codes) and the history", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { MyLeavePanel } = await import("./index");
    await renderScreen(<MyLeavePanel />, { path: "/", role: "faculty" });
    expect(document.querySelector("[data-slice='7B']")).toBeTruthy();
    expect(await screen.findByRole("option", { name: "Casual leave" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Submit application" })).toBeTruthy();
    expect(screen.getByText("History")).toBeTruthy();
    expect(document.querySelector("[data-form='leave-apply'] input[name='startDate']")).toBeTruthy();
  });

  it("shows one balance tile per entitled leave type in whole days", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { MyLeaveStats } = await import("./index");
    await renderScreen(<MyLeaveStats />, { path: "/", role: "faculty" });
    const tile = await waitFor(() => {
      const t = document.querySelector("[data-kpi='balance-CL']");
      if (!t?.textContent?.includes("Casual leave")) throw new Error("no CL balance tile yet");
      return t;
    });
    expect(tile.textContent).not.toContain(".00");
  });
});
