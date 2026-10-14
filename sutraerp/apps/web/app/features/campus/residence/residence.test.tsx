/** Slice 8B web: mock API rules, HTTP request shape and the hostel / transport admin tabs. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHttpResidenceApi, PATHS } from "./api";
import { createMockResidenceApi } from "./mock";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

const institution = { id: "mock-institution", name: "Mock University" } as never;

describe("8B residence API", () => {
  it("allocates a free bed, waitlists when full and keeps money as strings (mock)", async () => {
    const api = createMockResidenceApi();
    const [pending] = await api.applications("i", "under_review");
    const approved = await api.decideApplication(pending!.id, pending!.version, "approve");
    expect(approved.status).toBe("approved");
    const allocated = await api.allocate(approved.id, approved.version);
    expect(allocated.status).toBe("allocated");
    const resident = (await api.allocations("i")).find((a) => a.id === allocated.allocationId)!;
    expect(resident.feeAmount).toBe("45000.00");
    const b = await api.apply("i", { personKind: "student", personId: "stu-9" });
    const third = await api.allocate(b.id, b.version);
    expect(third.status).toBe("allocated");
    const c = await api.apply("i", { personKind: "student", personId: "stu-10" });
    expect((await api.allocate(c.id, c.version)).status).toBe("waitlisted");
  });

  it("keeps one live pass per passenger and replaces a lost one (mock)", async () => {
    const api = createMockResidenceApi();
    const [p] = await api.passengers("i", "active");
    await expect(api.issuePass(p!.id, { validFrom: "2026-06-01", validTo: "2026-12-31" })).rejects.toThrow();
    const [pass] = await api.passes("i", "active");
    const replacement = await api.replacePass(pass!.id, pass!.version, "Lost", "200.00");
    expect(replacement.kind).toBe("replacement");
    expect(replacement.feeAmount).toBe("200.00");
    expect((await api.passes("i", "replaced")).length).toBe(1);
  });

  it("sends If-Match on commands and Idempotency-Key on creates", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "a1", status: "approved", version: 3 }), { status: 200 });
    };
    const api = createHttpResidenceApi(fetchImpl, "");
    await api.decideApplication("a1", 2, "approve");
    const headers = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.applicationDecision.replace("{id}", "a1"));
    expect(headers["If-Match"]).toBe('"2"');
    expect(JSON.parse(String(calls[0]?.init.body)).decision).toBe("approve");
    await api.apply("inst", { personKind: "student", personId: "s" });
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    await api.beds("inst");
    expect(calls[2]?.url).toBe(PATHS.beds.replace("{id}", "inst"));
  });
});

describe("8B admin tabs", () => {
  it("renders the hostel tab with beds, applications and residents", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { HostelAdminTab } = await import("./index");
    await renderScreen(<HostelAdminTab institution={institution} />);
    expect(document.querySelector("[data-slice='8B']")).toBeTruthy();
    expect((await screen.findAllByText("HAP-000002")).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "New application" })).toBeTruthy();
  });

  it("renders the transport tab with routes and trips", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { TransportAdminTab } = await import("./index");
    await renderScreen(<TransportAdminTab institution={institution} />);
    expect((await screen.findAllByText("R-01 · City loop")).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Plan trip" })).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { TransportAdminTab } = await import("./index");
    await renderScreen(<TransportAdminTab institution={null} />);
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
