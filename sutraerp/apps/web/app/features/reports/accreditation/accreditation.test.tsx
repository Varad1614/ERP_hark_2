/** Slice 9E web (accreditation H09): mock API rules, HTTP request shape, action rules and the Admin → Reports → Accreditation tab. */
import { screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../../institution/types";
import { createHttpAccreditationApi, PATHS } from "./api";
import { createMockAccreditationApi } from "./mock";
import { cycleActions, frameworkActions, parseCriteria, recordActions, recordItemActions } from "./index";

const INST = { id: "inst-9e", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("9E accreditation (mock)", () => {
  it("versions a framework, plans a cycle and activates it (H-061, H-062)", async () => {
    const api = createMockAccreditationApi();
    await expect(api.createFramework("i", { code: "NIRF", name: "NIRF 2027", body: "NIRF", criteria: parseCriteria("TLR|Teaching|40|; RP|Research|40|") })).rejects.toThrow();
    let f = await api.createFramework("i", { code: "NIRF", name: "NIRF 2027", body: "NIRF", criteria: parseCriteria("TLR|Teaching|60|student.active_students; RP|Research|40|") });
    expect(frameworkActions(f)).toEqual(["submit"]);
    f = await api.submitFramework(f.id, f.version);
    expect(f.status).toBe("active");
    let c = await api.createCycle("i", { frameworkId: f.id, title: "NIRF 2027", periodStart: "2026-09-01", periodEnd: "2027-03-31" });
    expect(cycleActions(c)).toEqual(["plan"]);
    await expect(api.planCycle(c.id, c.version, [{ criterionCode: "TLR", owner: "Dean", dueOn: "2027-01-31" }])).rejects.toThrow();
    c = await api.planCycle(c.id, c.version, [
      { criterionCode: "TLR", owner: "Dean", dueOn: "2027-01-31" },
      { criterionCode: "RP", owner: "Research cell", dueOn: "2027-02-28" },
    ]);
    expect(cycleActions(c)).toEqual(["plan", "submit"]);
    c = await api.submitCycle(c.id, c.version);
    expect(c.status).toBe("active");
    expect(c.approvalState).toBe("approved");
  });

  it("closes an obligation only when every requirement is verified (H-063) and cites sources for evidence (H-064)", async () => {
    const api = createMockAccreditationApi();
    let ob = (await api.records("i")).find((r) => r.kind === "obligation")!;
    expect(recordActions(ob)).toEqual(["item", "submit", "cancel"]);
    await expect(api.submitRecord(ob.id, ob.version, "compliant")).rejects.toThrow();
    const open = ob.items.find((i) => i.status === "open")!;
    ob = await api.progressItem(open.id, open.version, { status: "done", evidenceRef: "Affidavit register" });
    const done = ob.items.find((i) => i.id === open.id)!;
    expect(recordItemActions(done, true)).toEqual(["verify"]);
    ob = await api.verifyItem(done.id, done.version);
    ob = await api.submitRecord(ob.id, ob.version, "compliant");
    expect(ob.status).toBe("closed");
    const cycle = (await api.cycles("i"))[0]!;
    await expect(api.createRecord("i", { kind: "evidence", title: "No cycle" })).rejects.toThrow();
    let ev = await api.createRecord("i", { kind: "evidence", title: "Placement records", cycleId: cycle.id, criterionCode: "C5" });
    ev = await api.recordCommand(ev.id, ev.version, "start");
    await expect(api.submitRecord(ev.id, ev.version, "approved_evidence")).rejects.toThrow();
    let cited = await api.createRecord("i", { kind: "evidence", title: "Governance evidence", cycleId: cycle.id, governanceEvidenceIds: ["g-1"] });
    cited = await api.recordCommand(cited.id, cited.version, "start");
    cited = await api.submitRecord(cited.id, cited.version, "approved_evidence");
    expect(cited.outcome).toBe("approved_evidence");
  });

  it("hands off the approved self-study report and then closes the cycle (H-071)", async () => {
    const api = createMockAccreditationApi();
    let cycle = (await api.cycles("i"))[0]!;
    await expect(api.closeCycle(cycle.id, cycle.version)).rejects.toThrow();
    const ss = (await api.records("i")).find((r) => r.kind === "self_study")!;
    expect(recordActions(ss)).toEqual(["handoff"]);
    const handed = await api.handoff(ss.id, ss.version, "NAAC-SSR-2027-0042");
    expect(handed.handoffReference).toBe("NAAC-SSR-2027-0042");
    expect(recordActions(handed)).toEqual([]);
    cycle = (await api.cycles("i"))[0]!;
    expect(cycle.status).toBe("submitted");
    cycle = await api.closeCycle(cycle.id, cycle.version);
    expect(cycle.status).toBe("closed");
  });
});

describe("9E accreditation action rules", () => {
  it("offers the next legal steps and parses criteria", () => {
    expect(frameworkActions({ status: "draft", approvalState: "pending" })).toEqual(["approve", "reject"]);
    expect(frameworkActions({ status: "superseded", approvalState: "approved" })).toEqual([]);
    expect(cycleActions({ status: "planning", approvalState: "pending", plan: [] })).toEqual(["approve", "reject"]);
    expect(cycleActions({ status: "submitted", approvalState: "approved", plan: [] })).toEqual(["close"]);
    expect(recordActions({ status: "open", kind: "readiness", handedOffAt: null })).toEqual(["submit", "cancel"]);
    expect(recordActions({ status: "pending_decision", kind: "finding", handedOffAt: null })).toEqual(["approve", "reject"]);
    expect(recordItemActions({ status: "open" }, false)).toEqual([]);
    expect(parseCriteria("C1|Curricular aspects|15|a.b, c.d; bad|x; C2|Teaching|85|")).toEqual([
      { code: "C1", title: "Curricular aspects", weight: 15, metricKeys: ["a.b", "c.d"] },
      { code: "C2", title: "Teaching", weight: 85, metricKeys: [] },
    ]);
  });
});

describe("9E accreditation HTTP API", () => {
  it("sends If-Match on commands, Idempotency-Key on creates and the documented paths", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "x1", status: "open", version: 4, items: [] }), { status: 200 });
    };
    const api = createHttpAccreditationApi(fetchImpl, "");
    await api.handoff("r1", 5, "SSR-1");
    await api.createRecord("inst", { kind: "obligation", title: "Anti-ragging" });
    await api.planCycle("c1", 2, [{ criterionCode: "C1", owner: "IQAC", dueOn: "2027-01-31" }]);
    await api.records("inst");
    const h0 = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.recordHandoff.replace("{id}", "r1"));
    expect(h0["If-Match"]).toBe('"5"');
    expect(JSON.parse(String(calls[0]?.init.body))).toEqual({ reference: "SSR-1" });
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    expect(JSON.parse(String(calls[2]?.init.body))).toEqual({ entries: [{ criterionCode: "C1", owner: "IQAC", dueOn: "2027-01-31" }] });
    expect(calls[3]?.url).toBe("/api/v1/services/institutions/inst/accreditation-records?limit=200");
  });
});

describe("9E Admin → Reports → Accreditation tab", () => {
  it("renders frameworks, cycles and records", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { AccreditationAdminTab } = await import("./index");
    await renderScreen(<AccreditationAdminTab institution={INST} />);
    expect(document.querySelector("[data-slice='9E']")).toBeTruthy();
    expect(await screen.findByText("NAAC v1 · NAAC Revised Accreditation Framework")).toBeTruthy();
    expect(await screen.findByText("AC-000001 · NAAC cycle 2026-27")).toBeTruthy();
    expect(await screen.findByText("ACR-000001 · UGC anti-ragging compliance")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Define framework" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "New record" })).toBeTruthy();
  });

  it("asks for an institution first", async () => {
    const renderScreen = (await import("../../../test/harness")).renderScreen;
    const { AccreditationAdminTab } = await import("./index");
    await renderScreen(<AccreditationAdminTab institution={null} />);
    expect(screen.getByText("No institution selected")).toBeTruthy();
  });
});
