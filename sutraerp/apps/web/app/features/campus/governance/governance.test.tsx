/** Slice 9E web (campus governance G00): mock API rules, HTTP request shape, action rules and the Admin → Campus → Governance tab. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../../institution/types";
import { createHttpGovernanceApi, PATHS } from "./api";
import { createMockGovernanceApi } from "./mock";
import { caseActions, configActions, evidenceActions, itemActions, parseKpiThresholds } from "./index";

const INST = { id: "inst-9e", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("9E governance (mock)", () => {
  it("runs an action plan to verified closure (G-006) and publishes its evidence (G-015)", async () => {
    const api = createMockGovernanceApi();
    let c = await api.createCase("i", { kind: "action_plan", title: "Monsoon readiness", domains: ["maintenance", "hostel"] });
    expect(c.status).toBe("draft");
    expect(caseActions(c)).toEqual(["start", "cancel"]);
    c = await api.caseCommand(c.id, c.version, "start");
    expect(caseActions(c)).toContain("submit");
    c = await api.addItem(c.id, { kind: "action", title: "Clear drains", domain: "maintenance" });
    await expect(api.addItem(c.id, { kind: "criterion", title: "Wrong kind" })).rejects.toThrow();
    const item = c.items[0]!;
    c = await api.progressItem(item.id, item.version, { status: "done" });
    await expect(api.submitCase(c.id, c.version, "completed")).rejects.toThrow();
    const done = c.items[0]!;
    await expect(api.verifyItem(done.id, done.version)).rejects.toThrow();
    c = await api.verifyItem(done.id, done.version, "WO-000123");
    expect(c.items[0]?.status).toBe("verified");
    c = await api.submitCase(c.id, c.version, "completed", "All actions verified");
    expect(c.status).toBe("closed");
    expect(c.approvalState).toBe("approved");
    expect(caseActions(c)).toEqual(["evidence"]);
    let e = await api.createEvidence("i", c.id, "Monsoon plan evidence");
    expect(evidenceActions(e)).toEqual(["publish"]);
    e = await api.publishEvidence(e.id, e.version);
    expect(e.accreditationReady).toBe(true);
    await expect(api.publishEvidence(e.id, e.version)).rejects.toThrow();
  });

  it("escalates an open case and refuses evidence of an open one (G-012, G-002)", async () => {
    const api = createMockGovernanceApi();
    const readiness = (await api.cases("i")).find((x) => x.kind === "readiness_assessment")!;
    await expect(api.createEvidence("i", readiness.id)).rejects.toThrow();
    const esc = await api.escalate(readiness.id, "Library gaps", "Two criteria below threshold");
    expect(esc.kind).toBe("escalation");
    expect(esc.parentId).toBe(readiness.id);
    expect(["high", "critical"]).toContain(esc.severity);
    await expect(api.submitCase(readiness.id, readiness.version, "excellent")).rejects.toThrow();
  });

  it("versions the configuration and reports the effective policy (G-014)", async () => {
    const api = createMockGovernanceApi();
    expect((await api.policy("i")).policy.minCriterionScore).toBe(3);
    let v = await api.createConfig("i", "readiness_threshold", { minCriterionScore: 4 });
    expect(v.versionNo).toBe(2);
    expect(configActions(v)).toEqual(["submit"]);
    v = await api.submitConfig(v.id, v.version);
    expect(v.status).toBe("effective");
    const all = await api.configs("i");
    expect(all.filter((c) => c.key === "readiness_threshold" && c.status === "effective")).toHaveLength(1);
    expect((await api.policy("i")).policy.minCriterionScore).toBe(4);
  });
});

describe("9E governance action rules", () => {
  it("offers the next legal steps", () => {
    expect(caseActions({ status: "open", kind: "kpi_review", approvalState: "not_requested" })).toEqual(["item", "refresh", "submit", "escalate", "cancel"]);
    expect(caseActions({ status: "pending_decision", kind: "risk", approvalState: "pending" })).toEqual(["approve", "reject", "escalate"]);
    expect(caseActions({ status: "cancelled", kind: "risk", approvalState: "not_requested" })).toEqual([]);
    expect(itemActions({ status: "open", kind: "criterion" }, true)).toEqual(["rate", "start", "done"]);
    expect(itemActions({ status: "done", kind: "action" }, true)).toEqual(["verify", "reopen"]);
    expect(itemActions({ status: "done", kind: "action" }, false)).toEqual([]);
    expect(configActions({ status: "draft", approvalState: "pending" })).toEqual(["approve", "reject"]);
    expect(configActions({ status: "effective", approvalState: "approved" })).toEqual([]);
    expect(parseKpiThresholds("campus.tickets_past_sla=2; bad; x=1.5; y=abc")).toEqual({ "campus.tickets_past_sla": 2, x: 1.5 });
  });
});

describe("9E governance HTTP API", () => {
  it("sends If-Match on commands, Idempotency-Key on creates and the documented paths", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "x1", status: "open", version: 4, items: [] }), { status: 200 });
    };
    const api = createHttpGovernanceApi(fetchImpl, "");
    await api.submitCase("c1", 3, "ready", "All criteria rated");
    await api.createCase("inst", { kind: "risk", title: "Fire safety" });
    await api.verifyItem("it1", 2, "EV-1");
    await api.cases("inst");
    const h0 = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.caseSubmit.replace("{id}", "c1"));
    expect(h0["If-Match"]).toBe('"3"');
    expect(JSON.parse(String(calls[0]?.init.body))).toEqual({ outcome: "ready", rationale: "All criteria rated" });
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    expect(calls[2]?.url).toBe("/api/v1/campus/governance-items/it1/verify");
    expect(calls[3]?.url).toBe("/api/v1/campus/institutions/inst/governance-cases?limit=200");
  });
});

describe("9E Admin → Campus → Governance tab", () => {
  it("renders cases, configuration and evidence", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { GovernanceAdminTab } = await import("./index");
    await renderScreen(<GovernanceAdminTab institution={INST} />);
    expect(document.querySelector("[data-slice='9E']")).toBeTruthy();
    expect(await screen.findByText("GOV-000001 · Campus readiness for NAAC cycle")).toBeTruthy();
    expect(await screen.findByText("Operations review Q2 evidence")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open case" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "New version" })).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { GovernanceAdminTab } = await import("./index");
    await renderScreen(<GovernanceAdminTab institution={null} />);
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
