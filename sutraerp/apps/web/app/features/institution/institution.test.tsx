/**
 * Batch 2 admin screens (Admin → Campus setup, Admin → Academics) in mock mode, plus the thin
 * API layer's HTTP conventions (CSRF, Idempotency-Key, If-Match, problem+json).
 */
import { ApiProblem } from "@sutra/api-client";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setCsrfToken } from "../../lib/api";
import { renderScreen } from "../../test/harness";
import Academics from "../../routes/admin/academics";
import CampusSetup from "../../routes/admin/campus-setup";
import { createHttpInstitutionApi, fill, PATHS, type InstitutionApi } from "./api";
import { exceptionActions, versionActions } from "./CalendarTab";
import { fmtDate, missingRequired } from "./forms";
import { InstitutionApiProvider, selectInstitution } from "./hooks";
import { lifecycleActions } from "./InstitutionTab";
import { createMockInstitutionApi } from "./mock";
import { blueprintTypes, orgTree } from "./OrganisationTab";
import { parseLevels, parseSlots } from "./CurriculumTabs";
import { groupReadiness } from "./ReadinessTab";
import type { OrgUnit } from "./types";

beforeEach(() => {
  window.sessionStorage.clear();
  window.localStorage.clear();
  selectInstitution(null);
});
afterEach(() => setCsrfToken(undefined));

function wrap(api: InstitutionApi, node: ReactNode) {
  return <InstitutionApiProvider api={api}>{node}</InstitutionApiProvider>;
}

function form(name: string): HTMLFormElement {
  const f = document.querySelector(`[data-form="${name}"]`);
  if (!f) throw new Error(`form ${name} not rendered`);
  return f as HTMLFormElement;
}

function fillForm(name: string, values: Record<string, string>) {
  const f = form(name);
  for (const [k, v] of Object.entries(values)) {
    const el = f.querySelector(`[name="${k}"]`) as HTMLInputElement | HTMLSelectElement | null;
    if (!el) throw new Error(`field ${k} missing in ${name}`);
    fireEvent.change(el, { target: { value: v } });
  }
  fireEvent.submit(f);
}

async function click(action: string, within_: ParentNode = document) {
  const b = await waitFor(() => {
    const el = within_.querySelector(`[data-action="${action}"]`);
    if (!el) throw new Error(`action ${action} not rendered`);
    return el as HTMLButtonElement;
  });
  fireEvent.click(b);
}

describe("institution API layer (HTTP)", () => {
  it("sends CSRF + Idempotency-Key on creates and If-Match on commands, maps problem+json", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const responses: Response[] = [
      new Response(JSON.stringify({ id: "i1", code: "SIT", name: "Sutra", timezone: "Asia/Kolkata", status: "draft", version: 1 }), { status: 201 }),
      new Response(JSON.stringify({ id: "y1", status: "open", version: 2 }), { status: 200 }),
      new Response(JSON.stringify({ type: "about:blank", title: "Conflict", status: 409, detail: "academic year cannot move from open to open", errorCode: "conflict" }), {
        status: 409,
        headers: { "content-type": "application/problem+json" },
      }),
    ];
    const api = createHttpInstitutionApi(async (url, init) => {
      calls.push({ url, init });
      return responses.shift() ?? new Response(null, { status: 500 });
    }, "");
    setCsrfToken("csrf-1");
    await api.registerInstitution({ code: "SIT", name: "Sutra", timezone: "Asia/Kolkata" });
    const h0 = calls[0]!.init.headers as Record<string, string>;
    expect(calls[0]!.url).toBe("/api/v1/institution/institutions");
    expect(calls[0]!.init.method).toBe("POST");
    expect(h0["X-CSRF-Token"]).toBe("csrf-1");
    expect(h0["Idempotency-Key"]).toMatch(/^[0-9a-f-]{36}$/);
    expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ code: "SIT", name: "Sutra", timezone: "Asia/Kolkata" });
    await api.openYear("y1", 1);
    const h1 = calls[1]!.init.headers as Record<string, string>;
    expect(calls[1]!.url).toBe("/api/v1/institution/academic-years/y1/open");
    expect(h1["If-Match"]).toBe('"1"');
    expect(h1["Idempotency-Key"]).toBeUndefined();
    const err = await api.openYear("y1", 2).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiProblem);
    expect((err as ApiProblem).problem.status).toBe(409);
    expect((err as ApiProblem).message).toContain("cannot move");
  });

  it("uses the real 2A/2B endpoints (commands, decommission body, curriculum revisions)", async () => {
    const calls: { url: string; init: RequestInit }[] = [];
    const api = createHttpInstitutionApi(async (url, init) => {
      calls.push({ url, init });
      return new Response(JSON.stringify({ id: "x", version: 2 }), { status: 200 });
    }, "");
    setCsrfToken("csrf-1");
    await api.institutionCommand("i1", 3, "submit");
    await api.institutionCommand("i1", 4, "suspend", "Audit");
    await api.campusCommand("c1", 5, "decommission", { reason: "Lease ends", effectiveOn: "2027-03-31" });
    await api.reviseCurriculum("k1", { changeSummary: "BoS 2027", effectiveFrom: "2027-07-01" });
    await api.curriculumVersionCommand("v1", 2, "retire", "Replaced");
    await api.orgUnitCommand("u1", 1, "activate");
    expect(calls.map((c) => `${c.init.method} ${c.url}`)).toEqual([
      "POST /api/v1/institution/institutions/i1/submit",
      "POST /api/v1/institution/institutions/i1/suspend",
      "POST /api/v1/institution/campuses/c1/decommission",
      "POST /api/v1/institution/curricula/k1/revisions",
      "POST /api/v1/institution/curriculum-versions/v1/retire",
      "POST /api/v1/institution/org-units/u1/activate",
    ]);
    expect(JSON.parse(String(calls[1]!.init.body))).toEqual({ reason: "Audit" });
    expect(JSON.parse(String(calls[2]!.init.body))).toEqual({ reason: "Lease ends", effectiveOn: "2027-03-31" });
    expect((calls[2]!.init.headers as Record<string, string>)["If-Match"]).toBe('"5"');
    expect((calls[3]!.init.headers as Record<string, string>)["Idempotency-Key"]).toMatch(/^[0-9a-f-]{36}$/);
    expect(fill(PATHS.term, { versionId: "v 1", termId: "t" })).toBe("/api/v1/institution/calendar-versions/v%201/terms/t");
    expect(() => fill(PATHS.term, { versionId: "v" })).toThrow(/termId/);
  });
});

describe("pure helpers", () => {
  it("offers the lifecycle actions of each status", () => {
    expect(lifecycleActions("draft")).toEqual(["configure"]);
    expect(lifecycleActions("configured")).toEqual(["submit"]);
    expect(lifecycleActions("pending_approval")).toContain("decide:approve");
    expect(lifecycleActions("active")).toEqual(["suspend", "archive"]);
    expect(lifecycleActions("suspended")).toEqual(["reactivate", "archive"]);
    expect(lifecycleActions("archived")).toEqual([]);
    expect(versionActions({ status: "published" })).toEqual(["revise", "retire"]);
    expect(versionActions({ status: "superseded" })).toEqual([]);
    expect(exceptionActions({ status: "in_effect" })).toEqual(["complete"]);
    expect(exceptionActions({ status: "completed" })).toEqual([]);
  });

  it("builds the org tree depth-first and groups readiness by area", () => {
    const u = (id: string, parentId: string | null, code: string): OrgUnit => ({ id, parentId, code, name: code, institutionId: "i", blueprintId: "b", unitType: "x", depth: 0, path: code, status: "active", version: 1, createdAt: "", updatedAt: "" });
    const tree = orgTree([u("c", "b", "C"), u("a", null, "A"), u("b", "a", "B"), u("o", "missing", "O")]);
    expect(tree.map((t) => `${t.unit.code}${t.depth}`)).toEqual(["A0", "B1", "C2", "O0"]);
    const groups = groupReadiness([
      { key: "calendar.version_published", area: "calendar", label: "x", required: true, status: "unmet" },
      { key: "campus.active_campus", area: "campus", label: "y", required: true, status: "met" },
    ]);
    expect(groups.map((g) => g.key)).toEqual(["campus", "calendar"]);
    expect(fmtDate("2026-06-01")).toBe("1 Jun 2026");
    expect(missingRequired([{ name: "code", label: "Code", required: true }], { code: " " })).toEqual({ code: "Code is required" });
    expect(blueprintTypes("University, faculty,  department")).toEqual([
      { key: "university", label: "University", allowRoot: true, allowedParentTypes: [] },
      { key: "faculty", label: "Faculty", allowRoot: false, allowedParentTypes: ["university", "faculty", "department"] },
      { key: "department", label: "Department", allowRoot: false, allowedParentTypes: ["university", "faculty", "department"] },
    ]);
    expect(parseLevels("ug:Undergraduate, PG")).toEqual([
      { kind: "level", key: "ug", name: "Undergraduate" },
      { kind: "level", key: "pg", name: "pg" },
    ]);
    expect(parseSlots("1:cs101:20, 2:MD201:4:multidisciplinary")).toEqual([
      { termNo: 1, slotCode: "CS101", title: "CS101", credits: 20, category: "major_core" },
      { termNo: 2, slotCode: "MD201", title: "MD201", credits: 4, category: "multidisciplinary" },
    ]);
  });
});

describe("Admin → Campus setup", () => {
  it("is administrator-only", async () => {
    await renderScreen(wrap(createMockInstitutionApi(), <CampusSetup />), { path: "/campus/:tab?", entry: "/campus", role: "student" });
    expect(await screen.findByText("This screen isn’t available")).toBeTruthy();
  });

  it("registers an institution from the screen and walks its lifecycle", async () => {
    const api = createMockInstitutionApi();
    await renderScreen(wrap(api, <CampusSetup />), { path: "/campus/:tab?", entry: "/campus" });
    expect(await screen.findByText("No institution yet")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Register institution" }));
    // Client-side required check first, then the API's 422 inline.
    fireEvent.submit(form("register-institution"));
    expect(await screen.findByText("Code is required")).toBeTruthy();
    fillForm("register-institution", { code: "s", name: "Sutra Institute of Technology" });
    expect(await screen.findByText(/code must be 2-32 characters/)).toBeTruthy();
    fillForm("register-institution", { code: "SIT", name: "Sutra Institute of Technology", timezone: "Asia/Kolkata" });
    await waitFor(() => expect(document.querySelector("[data-institution-status]")?.getAttribute("data-institution-status")).toBe("draft"));
    expect(screen.getAllByText(/Sutra Institute of Technology/).length).toBeGreaterThan(0);
    // Configure needs a complete profile (422 until then).
    await click("configure");
    expect(await screen.findByText(/profile incomplete/)).toBeTruthy();
    await click("profile");
    fillForm("institution-profile", {
      shortName: "SIT",
      registrationNumber: "REG-001",
      address: "1 Campus Road, Pune",
      contactPhone: "+91 20 1234 5678",
      officialEmail: "office@sit.edu",
      administratorEmail: "admin@sit.edu",
    });
    await waitFor(() => expect(document.querySelector("[data-profile-revision]")?.getAttribute("data-profile-revision")).toBe("1"));
    await click("configure");
    await waitFor(() => expect(document.querySelector("[data-institution-status]")?.getAttribute("data-institution-status")).toBe("configured"));
    // Submitting is refused while readiness is incomplete (409 surfaces as a toast).
    await click("submit");
    expect(await screen.findByText(/activation readiness incomplete/)).toBeTruthy();
    expect(document.querySelector("[data-institution-status]")?.getAttribute("data-institution-status")).toBe("configured");
  });

  it("shows the readiness checklist from every slice provider", async () => {
    const api = createMockInstitutionApi();
    const reg = await api.registerInstitution({ code: "RDY", name: "Ready College" });
    const inst = await api.reviseProfile(reg.id, reg.version, { shortName: "RC", institutionType: "college", registrationNumber: "R1", address: "Pune", contactPhone: "1", officialEmail: "o@rc.edu", administratorEmail: "a@rc.edu", academicPattern: "semester", workingDays: ["mon"] });
    selectInstitution(inst.id);
    await renderScreen(wrap(api, <CampusSetup />), { path: "/campus/:tab?", entry: "/campus" });
    fireEvent.click(await screen.findByText("Activation readiness"));
    const box = await waitFor(() => {
      const el = document.querySelector("[data-readiness]");
      if (!el) throw new Error("no checklist");
      return el as HTMLElement;
    });
    expect(box.getAttribute("data-ready")).toBe("false");
    for (const k of ["institution.profile_complete", "campus.active_campus", "curriculum.programme_published", "calendar.version_published", "calendar.assignment_effective"]) {
      expect(box.querySelector(`[data-readiness-item="${k}"]`)).toBeTruthy();
    }
    expect(box.querySelector('[data-readiness-item="institution.profile_complete"]')?.getAttribute("data-status")).toBe("met");
    expect(box.querySelector('[data-readiness-item="curriculum.school_model_published"]')?.getAttribute("data-status")).toBe("not_applicable");
    const calendarRow = box.querySelector('[data-readiness-item="calendar.version_published"]') as HTMLElement;
    expect(within(calendarRow).getByRole("link", { name: "Fix" }).getAttribute("href")).toBe("/academics/calendar");
  });

  it("registers and activates a campus, and builds the org-unit tree", async () => {
    const api = createMockInstitutionApi();
    const inst = await api.registerInstitution({ code: "CMP", name: "Campus College" });
    selectInstitution(inst.id);
    await renderScreen(wrap(api, <CampusSetup />), { path: "/campus/:tab?", entry: "/campus" });
    fireEvent.click(await screen.findByText("Campuses"));
    fireEvent.click(await screen.findByRole("button", { name: "Register campus" }));
    fillForm("new-campus", { code: "main", name: "Main campus", line1: "1 University Road", city: "Pune", state: "Maharashtra", postalCode: "411007" });
    expect(await screen.findByText(/Main campus/)).toBeTruthy();
    await click("campus-activate");
    await waitFor(async () => expect((await api.listCampuses(inst.id))[0]?.status).toBe("active"));

    fireEvent.click(screen.getByText("Organisation"));
    // Units are typed by the published blueprint.
    await waitFor(() => form("publish-blueprint"));
    fillForm("publish-blueprint", {});
    await waitFor(() => expect(document.querySelector("[data-blueprint]")).toBeTruthy());
    fireEvent.click(await screen.findByRole("button", { name: "Add unit" }));
    fillForm("new-org-unit", { code: "sit", name: "Sutra Institute" });
    await waitFor(() => expect(document.querySelector('[data-org-unit="SIT"]')).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Add unit" }));
    const root = (await api.listOrgUnits(inst.id))[0]!;
    fillForm("new-org-unit", { parentId: root.id, unitType: "department", code: "cse", name: "Computer Science" });
    await waitFor(() => expect(document.querySelector('[data-org-unit="CSE"]')?.getAttribute("data-depth")).toBe("1"));
  });
});

describe("Admin → Academics → Academic calendar", () => {
  it("establishes a calendar, publishes a version, opens the year, assigns it and simulates a change", async () => {
    const api = createMockInstitutionApi();
    const inst = await api.registerInstitution({ code: "CAL", name: "Calendar College" });
    selectInstitution(inst.id);
    await renderScreen(wrap(api, <Academics />), { path: "/academics/:tab?", entry: "/academics" });
    fireEvent.click(await screen.findByText("Academic calendar"));
    expect(await screen.findByText("No academic calendar yet")).toBeTruthy();
    fillForm("new-calendar", { code: "main", name: "Main academic calendar" });
    await waitFor(() => expect(document.querySelector('[data-calendar="MAIN"]')).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Plan academic year" }));
    fillForm("new-year", { label: "2026-27", startsOn: "2027-06-01", endsOn: "2027-05-01" });
    expect(await screen.findByText(/end date must be after the start date/)).toBeTruthy();
    fillForm("new-year", { label: "2026-27", startsOn: "2026-06-01", endsOn: "2027-05-31" });
    await waitFor(() => expect(document.querySelector('[data-year="2026-27"]')).toBeTruthy());

    fillForm("new-version", {});
    await waitFor(() => expect(document.querySelector('[data-version-panel="1"]')?.getAttribute("data-version-status")).toBe("draft"));
    // Publishing without terms is refused by validation.
    await click("version-publish");
    expect(await screen.findByText("add at least one term")).toBeTruthy();
    fillForm("new-term", { code: "t1", name: "Semester 1", sequence: "1", startsOn: "2026-06-15", endsOn: "2026-11-30" });
    await waitFor(() => expect(document.querySelector('[data-term="T1"]')).toBeTruthy());
    fillForm("new-key-date", { kind: "holiday", name: "Independence Day", startsOn: "2026-08-15" });
    await waitFor(() => expect(document.querySelector('[data-key-date="Independence Day"]')).toBeTruthy());
    await click("version-publish");
    await waitFor(() => expect(document.querySelector('[data-version-panel="1"]')?.getAttribute("data-version-status")).toBe("published"));
    expect(document.querySelector('[data-form="new-term"]')).toBeNull();

    await click("year-open");
    await waitFor(async () => expect((await api.listYears(inst.id))[0]?.status).toBe("open"));

    fillForm("new-assignment", { effectiveFrom: "2026-06-01" });
    await click("assignment-activate");
    await waitFor(async () => expect((await api.readiness(inst.id)).items.filter((i) => i.area === "calendar").every((i) => i.status === "met")).toBe(true));

    fillForm("simulate", { endsOn: "2026-10-31" });
    const sim = await waitFor(() => {
      const el = document.querySelector("[data-simulation]");
      if (!el) throw new Error("no simulation");
      return el as HTMLElement;
    });
    expect(sim.getAttribute("data-outcome")).toBe("unfavourable");
    expect(sim.querySelector('[data-impact="terms.outside_version"]')).toBeTruthy();
    // The preview changed nothing.
    expect((await api.listVersions((await api.listCalendars(inst.id))[0]!.id))[0]?.endsOn).toBe("2027-05-31");
    await click("simulation-accept", sim);
    await waitFor(() => expect(screen.getByText("Decision recorded")).toBeTruthy());

    // Revise → successor draft (version 2) under the same calendar.
    await click("version-revise");
    await waitFor(() => expect(document.querySelector('[data-version-panel="2"]')?.getAttribute("data-version-status")).toBe("draft"));
  });

  it("registers a programme and publishes it", async () => {
    const api = createMockInstitutionApi();
    const inst = await api.registerInstitution({ code: "PRG", name: "Programme College" });
    selectInstitution(inst.id);
    await renderScreen(wrap(api, <Academics />), { path: "/academics/:tab?", entry: "/academics" });
    await waitFor(() => form("new-structure"));
    fillForm("new-structure", {});
    await click("structure-publish");
    await waitFor(async () => expect((await api.listStructures(inst.id))[0]?.status).toBe("published"));
    fireEvent.click(await screen.findByText("Programmes"));
    fireEvent.click(await screen.findByRole("button", { name: "Register programme" }));
    fillForm("new-programme", { code: "btech", name: "B.Tech CSE", level: "ug", durationTerms: "2" });
    await waitFor(() => expect(document.querySelector('[data-programme="BTECH"]')).toBeTruthy());
    await click("programme-publish");
    await waitFor(async () => expect((await api.listProgrammes(inst.id))[0]?.status).toBe("published"));

    // Curriculum with course slots → publish version 1 → revise into a successor draft.
    fireEvent.click(document.querySelector('[data-tab-link="curriculum"]')!);
    await waitFor(() => form("new-curriculum"));
    fillForm("new-curriculum", { code: "cse-2026", title: "CSE 2026", effectiveFrom: "2026-07-01", slots: "1:CS101:20, 2:CS201:20" });
    await waitFor(() => expect(document.querySelector('[data-curriculum-version="1"]')).toBeTruthy());
    await click("curriculum-publish");
    await waitFor(async () => expect((await api.listCurriculumVersions((await api.listCurricula((await api.listProgrammes(inst.id))[0]!.id))[0]!.id))[0]?.status).toBe("published"));
    await click("curriculum-revise");
    fillForm("curriculum-revise", { changeSummary: "BoS 2027", effectiveFrom: "2027-07-01" });
    await waitFor(() => expect(document.querySelector('[data-curriculum-version="2"]')).toBeTruthy());
  });
});
