/** Slice 8C web (reports & analytics): mock API rules, HTTP request shape, action rules and the two Admin → Reports tabs. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../../institution/types";
import { createHttpAnalyticsApi, PATHS } from "./api";
import { createMockAnalyticsApi } from "./mock";
import { cycleActions, dashboardActions, figure, frameworkActions, packageActions, parseManualFields, snapshotActions, widgetActions } from "./index";

const INST = { id: "inst-8c", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("8C dashboards (mock)", () => {
  it("versions → review → publish → rollback, and renders the published figures", async () => {
    const api = createMockAnalyticsApi();
    let [d] = await api.dashboards("i");
    expect(d?.currentVersionNo).toBe(2);
    const widgets = (await api.widgets("i")).filter((w) => w.status === "active");
    d = await api.createVersion(d!.id, widgets.map((w) => w.id), "All widgets");
    expect(d.versions[0]?.status).toBe("draft");
    expect(dashboardActions(d)).toContain("submit");
    d = await api.dashboardCommand(d.id, d.version, "submit");
    await expect(api.createVersion(d.id, [widgets[0]!.id])).rejects.toThrow();
    d = await api.decideDashboard(d.id, d.version, "approve");
    d = await api.dashboardCommand(d.id, d.version, "publish");
    expect(d.currentVersionNo).toBe(3);
    expect(d.versions.find((v) => v.versionNo === 2)?.status).toBe("superseded");
    const view = await api.render(d.id);
    expect(view.versionNo).toBe(3);
    expect(view.widgets.find((w) => w.metricKey === "student.active_students")?.metric?.value).toBe("1240");
    await expect(api.rollback(d.id, d.version, 3, "Oops")).rejects.toThrow();
    d = await api.rollback(d.id, d.version, 2, "Figures under review");
    expect(d.currentVersionNo).toBe(2);
    expect((await api.render(d.id)).versionNo).toBe(2);
  });

  it("binds only known figures and keeps finance dashboards on finance.* keys (F-076)", async () => {
    const api = createMockAnalyticsApi();
    await expect(api.createWidget("i", { code: "W-X", title: "X", metricKey: "campus.nope" })).rejects.toThrow();
    const draft = await api.createWidget("i", { code: "W-PAY", title: "Payments", metricKey: "finance.payments_collected" });
    expect(draft.status).toBe("draft");
    expect(widgetActions(draft)).toEqual(["activate", "retire"]);
    const pay = await api.widgetCommand(draft.id, draft.version, "activate");
    const fin = await api.createDashboard("i", { code: "FIN-KPI", name: "Finance KPIs", purpose: "Bursar", category: "finance" });
    const spaces = (await api.widgets("i")).find((w) => w.metricKey === "campus.active_spaces")!;
    await expect(api.createVersion(fin.id, [spaces.id])).rejects.toThrow();
    const ok = await api.createVersion(fin.id, [pay.id]);
    expect(ok.versions[0]?.widgets[0]?.metricKey).toBe("finance.payments_collected");
    const a = await api.assign(fin.id, "administrator");
    await expect(api.assign(fin.id, "administrator")).rejects.toThrow();
    expect((await api.revoke(fin.id, a.id, a.version, "Moved")).status).toBe("revoked");
  });

  it("serves the library and infrastructure operational reports", async () => {
    const api = createMockAnalyticsApi();
    const lib = await api.report("i", "library");
    expect(lib.items.every((m) => m.key.startsWith("campus.library_"))).toBe(true);
    const infra = await api.report("i", "infrastructure");
    expect(infra.items[0]?.key).toBe("campus.active_buildings");
  });
});

describe("8C regulatory reporting (mock)", () => {
  it("collects → validates → packages → approves → submits → acknowledges → closes", async () => {
    const api = createMockAnalyticsApi();
    const cycle = (await api.cycles("i")).find((c) => c.status === "validating")!;
    let detail = await api.cycle(cycle.id);
    const [first] = detail.snapshots;
    expect(first?.issues.map((x) => x.key)).toEqual(["naac_grade"]);
    expect(snapshotActions(first!)).toEqual(["reject"]);
    await expect(api.snapshotCommand(first!.id, first!.version, "validate")).rejects.toThrow();
    const s = await api.collect(cycle.id, { naac_grade: "A+" });
    expect(s.issues).toEqual([]);
    expect((await api.snapshot(first!.id)).status).toBe("rejected");
    const v = await api.snapshotCommand(s.id, s.version, "validate");
    expect(v.status).toBe("validated");
    detail = await api.cycle(cycle.id);
    expect(cycleActions(detail)).toContain("package");
    let p = await api.createPackage(cycle.id);
    expect(p.number).toMatch(/^SUB-/);
    p = await api.packageCommand(p.id, p.version, "submit");
    expect(p.status).toBe("pending_approval");
    p = await api.decidePackage(p.id, p.version, "approve");
    p = await api.packageCommand(p.id, p.version, "submit");
    expect(p.status).toBe("submitted");
    p = await api.acknowledge(p.id, p.version, "NIRF-2026-0001");
    expect(p.acknowledgementRef).toBe("NIRF-2026-0001");
    detail = await api.cycle(cycle.id);
    expect(cycleActions(detail)).toEqual(["close"]);
    const closed = await api.closeCycle(cycle.id, detail.cycle.version);
    expect(closed.cycle.status).toBe("closed");
  });

  it("corrects a submitted package with a new snapshot and a linked package", async () => {
    const api = createMockAnalyticsApi();
    const cycle = (await api.cycles("i")).find((c) => c.status === "validating")!;
    const s = await api.collect(cycle.id, { naac_grade: "A" });
    await api.snapshotCommand(s.id, s.version, "validate");
    let p = await api.createPackage(cycle.id);
    p = await api.packageCommand(p.id, p.version, "submit");
    p = await api.decidePackage(p.id, p.version, "approve");
    p = await api.packageCommand(p.id, p.version, "submit");
    const corrected = await api.packageCommand(p.id, p.version, "correct", "Wrong staff count");
    expect(corrected.status).toBe("corrected");
    expect((await api.cycle(cycle.id)).cycle.status).toBe("collecting");
    const s2 = await api.collect(cycle.id, { naac_grade: "A" });
    await api.snapshotCommand(s2.id, s2.version, "validate");
    const again = await api.createPackage(cycle.id);
    expect(again.correctedFromId).toBe(corrected.id);
  });

  it("drafts, approves and activates a framework, then opens a cycle", async () => {
    const api = createMockAnalyticsApi();
    await expect(api.createFramework("i", { code: "AISHE", name: "AISHE", regulator: "AISHE", metricKeys: ["people.nope"] })).rejects.toThrow();
    let f = await api.createFramework("i", { code: "AISHE", name: "AISHE", regulator: "AISHE", metricKeys: ["student.active_students"], manualFields: parseManualFields("pwd_students|PwD students|required") });
    expect(frameworkActions(f)).toEqual(["submit"]);
    await expect(api.createCycle(f.id, { periodLabel: "2026-27" })).rejects.toThrow();
    f = await api.frameworkCommand(f.id, f.version, "submit");
    expect(frameworkActions(f)).toEqual(["approve", "reject"]);
    f = await api.decideFramework(f.id, f.version, "approve");
    f = await api.frameworkCommand(f.id, f.version, "activate");
    expect(frameworkActions(f)).toEqual(["cycle"]);
    const c = await api.createCycle(f.id, { periodLabel: "2026-27", dueOn: "2027-01-31" });
    expect(c.collectionPlan.manualFields[0]).toEqual({ key: "pwd_students", label: "PwD students", required: true });
    await expect(api.createCycle(f.id, { periodLabel: "2026-27" })).rejects.toThrow();
  });
});

describe("8C action rules and formatting", () => {
  it("offers the next legal steps", () => {
    expect(packageActions({ status: "draft" })).toEqual(["submit"]);
    expect(packageActions({ status: "pending_approval" })).toEqual(["approve", "reject"]);
    expect(packageActions({ status: "submitted" })).toEqual(["acknowledge", "correct"]);
    expect(packageActions({ status: "corrected" })).toEqual([]);
    expect(dashboardActions({ status: "draft", versions: [], currentVersionId: null })).toEqual(["version", "assign"]);
    expect(widgetActions({ status: "retired" })).toEqual([]);
  });

  it("formats figures by unit and parses manual fields", () => {
    expect(figure({ value: "1845000.00", unit: "inr" })).toBe("₹18,45,000.00");
    expect(figure({ value: "92.5", unit: "percent" })).toBe("92.5%");
    expect(figure({ value: "12", unit: "count" })).toBe("12");
    expect(figure(null)).toBe("—");
    expect(parseManualFields("naac_grade|NAAC grade|required; remarks|Remarks")).toEqual([
      { key: "naac_grade", label: "NAAC grade", required: true },
      { key: "remarks", label: "Remarks", required: false },
    ]);
  });
});

describe("8C reports HTTP API", () => {
  it("sends If-Match on commands, Idempotency-Key on creates and the documented paths", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "x1", status: "published", version: 4, items: [] }), { status: 200 });
    };
    const api = createHttpAnalyticsApi(fetchImpl, "");
    await api.rollback("d1", 3, 1, "Wrong figure");
    await api.createWidget("inst", { code: "W-A", title: "A", metricKey: "campus.active_spaces" });
    await api.revoke("d1", "a1", 2, "Moved");
    await api.collect("c1", { naac_grade: "A" });
    await api.metrics("inst", "campus");
    const h0 = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.rollback.replace("{id}", "d1"));
    expect(h0["If-Match"]).toBe('"3"');
    expect(JSON.parse(String(calls[0]?.init.body))).toEqual({ versionNo: 1, reason: "Wrong figure" });
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    expect(calls[2]?.url).toBe("/api/v1/services/dashboards/d1/assignments/a1/revoke");
    expect(JSON.parse(String(calls[3]?.init.body))).toEqual({ manualValues: { naac_grade: "A" } });
    expect(calls[4]?.url).toBe(`${PATHS.metrics.replace("{id}", "inst")}?domain=campus`);
  });
});

describe("8C Admin → Reports tabs", () => {
  it("renders the dashboards tab with the live view, dashboards, widgets and reports", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { DashboardsAdminTab } = await import("./index");
    await renderScreen(<DashboardsAdminTab institution={INST} />);
    expect(document.querySelector("[data-slice='8C']")).toBeTruthy();
    expect(await screen.findByText("Live view · Campus operations · v2")).toBeTruthy();
    expect((await screen.findAllByText("CAMPUS-OPS · Campus operations")).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "New dashboard" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Define widget" })).toBeTruthy();
  });

  it("renders the regulatory tab with frameworks, cycles, snapshot issues and packages", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { RegulatoryAdminTab } = await import("./index");
    await renderScreen(<RegulatoryAdminTab institution={INST} />);
    expect((await screen.findAllByText("RC-000002 · NIRF v1")).length).toBeGreaterThan(0);
    expect(await screen.findByText("required manual field missing")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Define framework" })).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { RegulatoryAdminTab } = await import("./index");
    await renderScreen(<RegulatoryAdminTab institution={null} />);
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
