/** Slice 8A web: mock API rules, HTTP request shape and the three Admin → Campus tabs. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../../institution/types";
import { createHttpFacilitiesApi, PATHS } from "./api";
import { createMockFacilitiesApi, minutes, overlaps } from "./mock";

const INST = { id: "inst-8a", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("8A facilities API", () => {
  it("compares booking windows in minutes", () => {
    expect(minutes("09:30")).toBe(570);
    expect(overlaps("09:00", "11:00", "10:00", "12:00")).toBe(true);
    expect(overlaps("09:00", "10:00", "10:00", "12:00")).toBe(false);
  });

  it("refuses overlapping bookings and over-capacity rooms (mock)", async () => {
    const api = createMockFacilitiesApi();
    const room = (await api.tree("i")).find((n) => n.code === "MAIN-B1-101")!;
    const date = "2030-01-07";
    const r = await api.book("i", { nodeId: room.id, date, startTime: "09:00", endTime: "10:00", attendees: 10, purpose: "Seminar" });
    expect(r.status).toBe("confirmed");
    await expect(api.book("i", { nodeId: room.id, date, startTime: "09:30", endTime: "10:30", attendees: 5, purpose: "Clash" })).rejects.toThrow();
    await expect(api.book("i", { nodeId: room.id, date, startTime: "11:00", endTime: "12:00", attendees: 500, purpose: "Too many" })).rejects.toThrow();
    const cancelled = await api.reservationCommand(r.id, r.version, "cancel", "Moved");
    expect(cancelled.status).toBe("cancelled");
    await expect(api.reservationCommand(r.id, cancelled.version, "cancel")).rejects.toThrow();
  });

  it("restricts a lab when equipment fails and blocks its sessions (mock)", async () => {
    const api = createMockFacilitiesApi();
    const [lab] = await api.labs("i");
    const detail = await api.lab(lab!.id);
    const eq = detail.equipment[0]!;
    await api.equipmentStatus(eq.id, eq.version, "out_of_service", "Fan broken");
    expect((await api.lab(lab!.id)).lab.status).toBe("restricted");
    const s = detail.sessions[0]!;
    await expect(api.sessionCommand(s.id, s.version, "ready")).rejects.toThrow();
  });

  it("sends If-Match on commands and Idempotency-Key on creates and bulk runs", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "t1", status: "assigned", version: 3, items: [] }), { status: 200 });
    };
    const api = createHttpFacilitiesApi(fetchImpl, "");
    await api.assignTicket("t1", 2, "tech.ravi");
    const headers = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.assign.replace("{id}", "t1"));
    expect(headers["If-Match"]).toBe('"2"');
    expect(JSON.parse(String(calls[0]?.init.body)).assignee).toBe("tech.ravi");
    await api.slaCheck("inst");
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    await api.tickets("inst", { breached: true });
    expect(calls[2]?.url).toContain("breached=true");
  });
});

describe("Admin → Campus 8A tabs", () => {
  it("shows the infrastructure hierarchy and room bookings", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { InfrastructureAdminTab } = await import("./index");
    await renderScreen(<InfrastructureAdminTab institution={INST} />, { path: "/" });
    expect(document.querySelector("[data-slice='8A']")).toBeTruthy();
    expect(await screen.findByText("Room 101")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Book a space" })).toBeTruthy();
  });

  it("shows the ticket queue with SLA badges", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { FacilitiesAdminTab } = await import("./index");
    await renderScreen(<FacilitiesAdminTab institution={INST} />, { path: "/" });
    expect(await screen.findByText("SR-000001")).toBeTruthy();
    expect(screen.getByText("Breached · L1")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Run SLA check" })).toBeTruthy();
  });

  it("shows laboratories with their sessions", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { LabsAdminTab } = await import("./index");
    await renderScreen(<LabsAdminTab institution={INST} />, { path: "/" });
    expect((await screen.findAllByText("Chemistry lab")).length).toBeGreaterThan(0);
    expect(await screen.findByText("CHE101 · Titration")).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { LabsAdminTab } = await import("./index");
    await renderScreen(<LabsAdminTab institution={null} />, { path: "/" });
    expect(screen.getByText("No institution selected")).toBeTruthy();
    expect(document.querySelector("[data-slice='8A']")).toBeTruthy();
  });
});
