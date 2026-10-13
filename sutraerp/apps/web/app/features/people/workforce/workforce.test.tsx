/** Slice 7A web: mock API behaviour, HTTP request shape, action rules and the My record panel. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHttpWorkforceApi, PATHS } from "./api";
import { createMockWorkforceApi } from "./mock";
import { engagementActions, staffActions } from "./index";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("7A workforce API", () => {
  it("walks a staff record through verification and an engagement to active (mock)", async () => {
    const api = createMockWorkforceApi();
    const s = await api.createStaff("inst-1", { givenName: "Anika", familyName: "Rao", workEmail: "anika@b7a.dev", staffCategory: "teaching" });
    expect(s.status).toBe("draft");
    await expect(api.createStaff("inst-1", { givenName: "A", workEmail: "anika@b7a.dev", staffCategory: "teaching" })).rejects.toThrow();
    const e = await api.createEngagement(s.id, { employmentType: "permanent", designation: "Lecturer", startDate: "2026-07-01" });
    await expect(api.engagementCommand(e.id, e.version, "submit")).rejects.toThrow();
    let r = await api.staffCommand(s.id, s.version, "submit-verification");
    r = await api.staffCommand(r.id, r.version, "verify");
    r = await api.staffCommand(r.id, r.version, "activate");
    expect(r.status).toBe("active");
    const active = await api.engagementCommand(e.id, e.version, "submit");
    expect(active.status).toBe("active");
    expect((await api.staffRecord(s.id)).engagement?.status).toBe("active");
  });

  it("refuses a second occupant of a single-occupant position (mock)", async () => {
    const api = createMockWorkforceApi();
    const [p] = await api.positions("inst-1");
    await expect(api.assign(p?.id ?? "", { engagementId: "eng-1", startDate: "2026-07-01" })).rejects.toThrow();
  });

  it("sends If-Match and Idempotency-Key", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "e1", status: "active", version: 3 }), { status: 200 });
    };
    const api = createHttpWorkforceApi(fetchImpl, "");
    await api.engagementCommand("e1", 2, "submit");
    await api.createEngagement("s1", { employmentType: "permanent", designation: "Lecturer", startDate: "2026-07-01" });
    const h0 = calls[0]?.init.headers as Record<string, string>;
    const h1 = calls[1]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.submit.replace("{id}", "e1"));
    expect(h0["If-Match"]).toBe('"2"');
    expect(h1["Idempotency-Key"]).toBeTruthy();
  });

  it("offers the next step of each record", () => {
    expect(staffActions({ status: "draft" })).toEqual(["submit-verification"]);
    expect(staffActions({ status: "verified" })).toEqual(["activate"]);
    expect(engagementActions({ status: "pending", pendingChange: null })).toEqual(["submit", "cancel"]);
    expect(engagementActions({ status: "pending", pendingChange: { id: "c", kind: "activation", proposal: {}, status: "pending_approval" } })).toEqual(["approve", "reject"]);
    expect(engagementActions({ status: "active", pendingChange: null })).toEqual(["suspend", "close"]);
  });
});

describe("My record panel", () => {
  it("shows the faculty member's record and qualifications", async () => {
    const { renderScreen } = await import("../../../test/harness");
    const { MyProfilePanel } = await import("./index");
    await renderScreen(<MyProfilePanel />, { role: "faculty", path: "/leave", entry: "/leave" });
    expect(await screen.findByText("PhD Computer Science (2019)")).toBeTruthy();
    expect(document.querySelector("[data-slice='7A']")).toBeTruthy();
  });
});
