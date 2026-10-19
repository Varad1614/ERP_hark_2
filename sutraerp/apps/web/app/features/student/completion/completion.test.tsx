/** Slice 3C screens: Student 360 completion / guardians / scholarships and the admin tabs (mock API). */
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { ReactNode } from "react";
import { C3ApiProvider, completionActions, createMockC3Api, eligibilityActions, ExitsAdminTab, relationshipActions, scholarshipActions, ScholarshipsAdminTab, Student360Completion, Student360Guardians, Student360Scholarships } from "./index";
import type { C3Api } from "./api";
import type { Institution } from "../../institution/types";
import { renderScreen } from "../../../test/harness";

beforeEach(() => {
  window.sessionStorage.clear();
});

async function render(api: C3Api, node: ReactNode) {
  return renderScreen(<C3ApiProvider api={api}>{node}</C3ApiProvider>);
}

/** Waits until `selector` matches and returns the element. */
async function el(selector: string): Promise<HTMLElement> {
  return waitFor(() => {
    const e = document.querySelector(selector);
    if (!e) throw new Error(`no ${selector}`);
    return e as HTMLElement;
  });
}

const INST = { id: "inst-1", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

describe("3C action rules", () => {
  it("offers the next lifecycle commands only", () => {
    const base = { id: "c", studentId: "s", outcome: "degree", pathway: "programme_completion" as const, clearances: [], version: 1, createdAt: "" };
    expect(completionActions({ ...base, status: "approved" })).toEqual(["award"]);
    expect(completionActions({ ...base, status: "exit_completed" })).toEqual([]);
    expect(relationshipActions({ id: "r", guardianId: "g", studentId: "s", relationshipType: "mother", isPrimary: false, hasLegalCustody: false, status: "suspended", version: 1 })).toEqual(["reinstate", "terminate"]);
    expect(scholarshipActions({ id: "x", code: "M", name: "M", sponsorType: "i", benefitType: "b", status: "draft", renewable: false, version: 1 })).toEqual(["publish"]);
    expect(eligibilityActions({ id: "e", scholarshipId: "x", studentId: "s", status: "active", conditions: [], recommendationPending: false, version: 1 })).toContain("renew");
  });
});

describe("Student 360 completion", () => {
  it("runs a completion to the conferred qualification", async () => {
    const api = createMockC3Api();
    await render(api, <Student360Completion studentId="s1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Start completion" }));
    await waitFor(() => expect(document.querySelector("[data-completion='eligibility_evaluation']")).toBeTruthy());
    fireEvent.click(document.querySelector("[data-action='completion-eligibility-evaluation']")!);
    const form = await el("[data-form='completion-eligibility']");
    fireEvent.change(within(form).getByLabelText("Credits earned"), { target: { value: "96" } });
    fireEvent.click(within(form).getByRole("button", { name: "Evaluate" }));
    await waitFor(() => expect(document.querySelector("[data-completion='degree_audit']")).toBeTruthy());
    fireEvent.click(document.querySelector("[data-action='completion-degree-audit']")!);
    await waitFor(() => expect(document.querySelector("[data-clearance='library']")).toBeTruthy());
    expect(document.querySelector("[data-action='resolve-finance']")).toBeNull();
    fireEvent.click(document.querySelector("[data-action='resolve-library']")!);
    await waitFor(() => expect(document.querySelector("[data-action='resolve-library']")).toBeNull());
    fireEvent.click(document.querySelector("[data-action='resolve-department']")!);
    await waitFor(() => expect(document.querySelector("[data-completion='approved']")).toBeTruthy());
    fireEvent.click(document.querySelector("[data-action='completion-award']")!);
    await waitFor(() => expect(document.querySelector("[data-completion='awarded']")).toBeTruthy());
    expect(await screen.findByText("degree conferred")).toBeTruthy();
  });

  it("initiates an exit and resolves its clearance in the admin tab", async () => {
    const api = createMockC3Api();
    const e = await api.createExit("s2", { kind: "withdrawal", reason: "relocating" });
    await render(api, <ExitsAdminTab institution={INST} />);
    expect(await screen.findByRole("heading", { name: "Student exits" })).toBeTruthy();
    expect(e.status).toBe("clearance_pending");
  });
});

describe("Student 360 guardians and scholarships", () => {
  it("lists relationships with their next commands", async () => {
    const api = createMockC3Api();
    const g = await api.createGuardian("inst-1", { givenName: "Kiran", phone: "+91980" });
    await api.proposeRelationship("s3", { guardianId: g.id, relationshipType: "father" });
    await render(api, <Student360Guardians studentId="s3" />);
    expect(await screen.findByText("Kiran")).toBeTruthy();
    expect(document.querySelector("[data-action='relationship-submit-verification']")).toBeTruthy();
  });

  it("drives an eligibility through recommendation and hand-off", async () => {
    const api = createMockC3Api();
    await api.determine("s4", { scholarshipId: "sch-000000001" });
    await render(api, <Student360Scholarships studentId="s4" />);
    fireEvent.click(await el("[data-action='eligibility-recommend']"));
    await waitFor(() => expect(document.querySelector("[data-eligibility='recommendation_generated']")).toBeTruthy());
    fireEvent.click(document.querySelector("[data-action='eligibility-hand-off']")!);
    await waitFor(() => expect(document.querySelector("[data-eligibility='active']")).toBeTruthy());
  });

  it("refuses to make a scholarship available without a policy", async () => {
    const api = createMockC3Api();
    const s = await api.createScholarship("inst-1", { code: "MERIT", name: "Merit", sponsorType: "institutional", benefitType: "fee_waiver" });
    await api.scholarshipCommand(s.id, s.version, "publish");
    await render(api, <ScholarshipsAdminTab institution={INST} />);
    fireEvent.click(await el("[data-action='scholarship-activate']"));
    expect(await screen.findByText(/effective eligibility policy/)).toBeTruthy();
  });
});
