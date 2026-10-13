/** Slice 7C web: mock API lifecycle, HTTP request shape, the admin Payroll tab and My payslips. */
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createHttpPayrollApi, PATHS } from "./api";
import { runActions } from "./hooks";
import { createMockPayrollApi } from "./mock";

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

async function harness() {
  return (await import("../../../test/harness")).renderScreen;
}

const institution = { id: "00000000-0000-7000-8000-0000000b7c00", name: "Mock University" } as never;

describe("7C payroll API", () => {
  it("moves a run through its lifecycle and refuses out-of-order commands (mock)", async () => {
    const api = createMockPayrollApi();
    const [run] = await api.runs("per-1");
    expect(run?.status).toBe("calculated");
    expect(run?.netPay).toBe("41800.00");
    await expect(api.runCommand(run!.id, run!.version, "post")).rejects.toThrow();
    await expect(api.runCommand(run!.id, run!.version + 1, "validate")).rejects.toThrow();
    const validated = await api.runCommand(run!.id, run!.version, "validate");
    expect(validated.status).toBe("validating");
    await expect(api.createRun("per-1", "regular")).rejects.toThrow();
    const retro = await api.createRun("per-1", "retro");
    expect(retro.status).toBe("created");
    expect(retro.number).toMatch(/^PR-\d{6}$/);
  });

  it("activates one configuration at a time (mock)", async () => {
    const api = createMockPayrollApi();
    const draft = await api.createConfig("inst-1", { name: "Payroll 2027", effectiveFrom: "2027-04-01", copyFrom: "cfg-1" });
    expect(draft.status).toBe("draft");
    await expect(api.configCommand(draft.id, draft.version, "activate")).rejects.toThrow();
    const approved = await api.configCommand(draft.id, draft.version, "submit");
    const active = await api.configCommand(approved.id, approved.version, "activate");
    expect(active.status).toBe("active");
    const all = await api.configs("inst-1");
    expect(all.filter((c) => c.status === "active")).toHaveLength(1);
    expect(all.find((c) => c.id === "cfg-1")?.status).toBe("superseded");
  });

  it("offers the next commands for a run status", () => {
    expect(runActions("created")).toEqual(["calculate"]);
    expect(runActions("pending_approval")).toEqual(["approve", "reject"]);
    expect(runActions("finalized")).toEqual(["post", "issue"]);
    expect(runActions("closed")).toEqual([]);
  });

  it("sends If-Match on commands and Idempotency-Key on creates", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "r1", status: "approved", version: 4 }), { status: 200 });
    };
    const api = createHttpPayrollApi(fetchImpl, "");
    await api.decideRun("r1", 3, "approve");
    const headers = calls[0]?.init.headers as Record<string, string>;
    expect(calls[0]?.url).toBe(PATHS.decision.replace("{id}", "r1"));
    expect(headers["If-Match"]).toBe('"3"');
    expect(JSON.parse(String(calls[0]?.init.body)).decision).toBe("approve");
    await api.createRun("p1", "regular");
    expect(calls[1]?.url).toBe(PATHS.runs.replace("{id}", "p1"));
    expect((calls[1]?.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
  });

  it("turns a problem response into an error", async () => {
    const fetchImpl = async () =>
      new Response(JSON.stringify({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict" }), { status: 409 });
    const api = createHttpPayrollApi(fetchImpl, "");
    await expect(api.runCommand("r1", 1, "post")).rejects.toThrow();
  });
});

describe("7C screens (mock API)", () => {
  it("renders the admin Payroll tab and validates the calculated run", async () => {
    const renderScreen = await harness();
    const { PayrollAdminTab } = await import("./index");
    await renderScreen(<PayrollAdminTab institution={institution} />);
    expect(document.querySelector("[data-slice='7C']")).toBeTruthy();
    expect(await screen.findByText(/v1 · Payroll 2026/)).toBeTruthy();
    expect((await screen.findAllByText(/PR-000001/)).length).toBeGreaterThan(0);
    expect((await screen.findAllByText("₹41800.00")).length).toBeGreaterThan(0);
    expect((await screen.findAllByText(/EMP-000001 · Meera Shah/)).length).toBeGreaterThan(0);
    const validate = await waitFor(() => {
      const b = document.querySelector<HTMLButtonElement>("[data-action='run-validate']");
      if (!b) throw new Error("no validate action");
      return b;
    });
    fireEvent.click(validate);
    await waitFor(() => expect(document.querySelector("[data-action='run-submit']")).toBeTruthy());
  });

  it("asks for an institution first", async () => {
    const renderScreen = await harness();
    const { PayrollAdminTab } = await import("./index");
    await renderScreen(<PayrollAdminTab institution={null} />);
    expect(await screen.findByText("No institution selected")).toBeTruthy();
  });

  it("shows the caller's own payslips in My HR", async () => {
    const renderScreen = await harness();
    const { MyPayslipsPanel } = await import("./index");
    await renderScreen(<MyPayslipsPanel />, { path: "/", role: "faculty" });
    expect(document.querySelector("[data-slice='7C']")).toBeTruthy();
    expect(await screen.findByText("PS-000001")).toBeTruthy();
    expect(screen.getAllByText("₹40200.00").length).toBeGreaterThan(0);
  });

  it("shows an empty state when there are no payslips", async () => {
    const renderScreen = await harness();
    const { MyPayslipsPanel, PayrollApiProvider } = await import("./index");
    const api = { ...createMockPayrollApi(), myPayslips: async () => [] };
    await renderScreen(
      <PayrollApiProvider api={api}>
        <MyPayslipsPanel />
      </PayrollApiProvider>,
      { path: "/", role: "faculty" },
    );
    expect(await screen.findByText("No payslips yet")).toBeTruthy();
  });
});
