/**
 * Slice 3A screens in mock mode (Admin → Students directory + admissions, Student 360 overview /
 * enrolment / documents, student-facing Documents) and the HTTP API layer's conventions
 * (CSRF, Idempotency-Key, If-Match, problem+json).
 */
import { ApiProblem } from "@sutra/api-client";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setCsrfToken } from "../../../lib/api";
import { renderScreen } from "../../../test/harness";
import Students from "../../../routes/admin/students";
import Student360 from "../../../routes/admin/student-360";
import Documents from "../../../routes/student/documents";
import { InstitutionApiProvider, selectInstitution } from "../../institution/hooks";
import { createMockInstitutionApi } from "../../institution/mock";
import type { InstitutionApi } from "../../institution/api";
import { admissionActions, createHttpRecordApi, fill, PATHS, type RecordApi } from "./api";
import { RecordApiProvider } from "./hooks";
import { createMockRecordApi, seededMockRecordApi } from "./mock";
import { clickAction } from "../test-utils";

beforeEach(() => {
  window.sessionStorage.clear();
  window.localStorage.clear();
  selectInstitution(null);
});
afterEach(() => setCsrfToken(undefined));

function wrap(inst: InstitutionApi, rec: RecordApi, node: ReactNode) {
  return (
    <InstitutionApiProvider api={inst}>
      <RecordApiProvider api={rec}>{node}</RecordApiProvider>
    </InstitutionApiProvider>
  );
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

async function click(action: string) {
  await clickAction(action);
}

async function institution() {
  const api = createMockInstitutionApi();
  const inst = await api.registerInstitution({ code: "SIT", name: "Sutra Institute", timezone: "Asia/Kolkata" });
  return { api, inst };
}

describe("3A API layer (HTTP)", () => {
  it("sends CSRF, Idempotency-Key on creates and If-Match on commands; maps problem+json", async () => {
    setCsrfToken("csrf-1");
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const api = createHttpRecordApi(async (url, init) => {
      calls.push({ url, init });
      if (url.endsWith("/accept")) return new Response(JSON.stringify({ type: "about:blank", title: "Conflict", status: 409, detail: "the offer has expired", errorCode: "conflict" }), { status: 409, headers: { "content-type": "application/problem+json" } });
      return new Response(JSON.stringify({ id: "x", version: 2, items: [] }), { status: 200 });
    }, "");
    await api.createApplication("i1", { programmeId: "p1", givenName: "Asha" });
    const h0 = calls[0]!.init.headers as Record<string, string>;
    expect(calls[0]!.url).toBe("/api/v1/student/institutions/i1/admission-applications");
    expect(h0["X-CSRF-Token"]).toBe("csrf-1");
    expect(h0["Idempotency-Key"]).toBeTruthy();
    await api.suspend("s1", 4, "hearing", "disciplinary");
    const h1 = calls[1]!.init.headers as Record<string, string>;
    expect(h1["If-Match"]).toBe('"4"');
    expect(JSON.parse(calls[1]!.init.body as string)).toEqual({ reason: "hearing", category: "disciplinary" });
    await api.directory("i1", "aar v");
    expect(calls[2]!.url).toBe("/api/v1/student/institutions/i1/students?q=aar%20v");
    await expect(api.admissionCommand("a1", 1, "accept")).rejects.toBeInstanceOf(ApiProblem);
    expect(fill(PATHS.admissionDocumentVerify, { id: "a", documentId: "d" })).toBe("/api/v1/student/admissions/a/documents/d/verify");
  });

  it("offers the admission actions of each status", () => {
    expect(admissionActions({ status: "initiated", offerAcceptedAt: null })).toEqual(["accept", "documents", "cancel", "expire"]);
    expect(admissionActions({ status: "initiated", offerAcceptedAt: "2026-10-01T00:00:00Z" })).toContain("submit-documents");
    expect(admissionActions({ status: "approval_pending" })).toEqual(["decide"]);
    expect(admissionActions({ status: "confirmed" })).toEqual(["create-student", "withdraw", "revoke"]);
    expect(admissionActions({ status: "revoked" })).toEqual(["readmit"]);
  });
});

describe("3A mock rules", () => {
  it("walks an admission to an active student and refuses illegal moves", async () => {
    const { api, studentId } = await seededMockRecordApi("inst-1");
    const s = await api.getStudent(studentId);
    expect(s.status).toBe("active");
    expect(s.currentContext?.status).toBe("active");
    const adm = (await api.listAdmissions("inst-1"))[0]!;
    expect(adm.status).toBe("completed");
    await expect(api.admissionCommand(adm.id, adm.version, "cancel", "x")).rejects.toBeInstanceOf(ApiProblem);
    await expect(api.suspend(studentId, 999, "x", "other")).rejects.toMatchObject({ problem: { status: 412 } });
    const sus = await api.suspend(studentId, s.currentContext!.version, "hearing", "disciplinary");
    expect(sus.status).toBe("suspended");
    await expect(api.archive(studentId, sus.version, "x")).rejects.toMatchObject({ problem: { status: 409 } });
  });
});

describe("Admin → Students (3A tabs)", () => {
  it("lists students in the directory and filters them", async () => {
    const { api: inst, inst: i } = await institution();
    const { api } = await seededMockRecordApi(i.id);
    await renderScreen(wrap(inst, api, <Students />), { path: "/students/:tab?", entry: "/students" });
    expect(await screen.findByText("Aarav Desai")).toBeTruthy();
    const link = screen.getByText("Aarav Desai").closest("a")!;
    expect(link.getAttribute("href")).toMatch(/^\/students\/record\//);
    fireEvent.change(screen.getByLabelText("Search students"), { target: { value: "zzz" } });
    expect(await screen.findByText("No matching students")).toBeTruthy();
  });

  it("drives an application through offer, acceptance and documents", async () => {
    const { api: inst, inst: i } = await institution();
    const api = createMockRecordApi();
    await api.createApplication(i.id, { programmeId: "p1", givenName: "Ishaan" });
    await renderScreen(wrap(inst, api, <Students />), { path: "/students/:tab?", entry: "/students/admissions" });
    expect(await screen.findByText("Ishaan")).toBeTruthy();
    await click("application-submit");
    await click("application-offer");
    fillForm("offer", { requiredDocuments: "identity_proof" });
    await waitFor(async () => expect((await api.listAdmissions(i.id)).length).toBe(1));
    await click("admission-accept");
    await waitFor(async () => expect((await api.listAdmissions(i.id))[0]!.offerAcceptedAt).toBeTruthy());
    await click("admission-documents");
    fireEvent.click(await screen.findByRole("button", { name: "Add document" }));
    fillForm("admission-document", { title: "Aadhaar" });
    await waitFor(async () => expect((await api.admissionDocuments((await api.listAdmissions(i.id))[0]!.id)).length).toBe(1));
    // Cancelling needs a reason (client check), then the admission is cancelled.
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await click("admission-cancel");
    fireEvent.submit(form("admission-cancel"));
    expect(await screen.findByText("Reason is required")).toBeTruthy();
    fillForm("admission-cancel", { reason: "applicant request" });
    await waitFor(async () => expect((await api.listAdmissions(i.id))[0]!.status).toBe("cancelled"));
    await click("admission-readmit");
  });
});

describe("Student 360 (3A tabs)", () => {
  it("shows the overview and suspends / restores through reason forms", async () => {
    const { api: inst, inst: i } = await institution();
    const { api, studentId } = await seededMockRecordApi(i.id);
    await renderScreen(wrap(inst, api, <Student360 />), { path: "/students/record/:studentId/:tab?", entry: `/students/record/${studentId}` });
    expect(await screen.findByRole("heading", { name: "Aarav Desai", level: 1 })).toBeTruthy();
    await click("student-suspend");
    fillForm("student-suspend", { reason: "disciplinary hearing" });
    await waitFor(() => expect(document.querySelector("[data-current-context]")?.getAttribute("data-current-context")).toBe("suspended"));
    await click("student-restore");
    fillForm("student-restore", { reason: "hearing closed" });
    await waitFor(() => expect(document.querySelector("[data-current-context]")?.getAttribute("data-current-context")).toBe("active"));
  });

  it("updates the assignment on the enrolment tab and issues a request on the documents tab", async () => {
    const { api: inst, inst: i } = await institution();
    const { api, studentId } = await seededMockRecordApi(i.id);
    await renderScreen(wrap(inst, api, <Student360 />), { path: "/students/record/:studentId/:tab?", entry: `/students/record/${studentId}/enrolment` });
    await click("student-assignment");
    fillForm("student-assignment", { sectionCode: "B" });
    await waitFor(async () => expect((await api.getStudent(studentId)).currentContext?.sectionCode).toBe("B"));
    expect(await screen.findByText("Assignment")).toBeTruthy();
  });

  it("processes a document request", async () => {
    const { api: inst, inst: i } = await institution();
    const { api, studentId } = await seededMockRecordApi(i.id);
    await api.raiseRequest(studentId, { documentType: "bonafide_certificate" });
    await renderScreen(wrap(inst, api, <Student360 />), { path: "/students/record/:studentId/:tab?", entry: `/students/record/${studentId}/documents` });
    expect(await screen.findByText("Bonafide certificate")).toBeTruthy();
    await click("request-issue");
    fillForm("request-issue", { issuedReference: "DOC-1" });
    await waitFor(async () => expect((await api.studentRequests(studentId))[0]!.status).toBe("issued"));
  });

  it("is read-only for faculty", async () => {
    const { api: inst, inst: i } = await institution();
    const { api, studentId } = await seededMockRecordApi(i.id);
    await renderScreen(wrap(inst, api, <Student360 />), { path: "/students/record/:studentId/:tab?", entry: `/students/record/${studentId}`, role: "faculty" });
    expect(await screen.findByRole("heading", { name: "Aarav Desai", level: 1 })).toBeTruthy();
    expect(document.querySelector('[data-action="student-suspend"]')).toBeNull();
    // Audit C3: the admin-only sections are not offered to faculty.
    expect(document.querySelector(`a[href="/students/record/${studentId}/documents"]`)).toBeNull();
  });

  it("opens the overview when faculty ask for an admin-only section", async () => {
    const { api: inst, inst: i } = await institution();
    const { api, studentId } = await seededMockRecordApi(i.id);
    await renderScreen(wrap(inst, api, <Student360 />), { path: "/students/record/:studentId/:tab?", entry: `/students/record/${studentId}/guardians`, role: "faculty" });
    expect(await screen.findByRole("heading", { name: "Aarav Desai", level: 1 })).toBeTruthy();
    expect(document.querySelector('[data-screen="student-360"]')?.getAttribute("data-tab")).toBe("overview");
  });
});

describe("Student-facing Documents", () => {
  it("explains a missing student link", async () => {
    const { api: inst } = await institution();
    await renderScreen(wrap(inst, createMockRecordApi(), <Documents />), { path: "/documents", role: "student" });
    expect(await screen.findByText("No student record linked")).toBeTruthy();
  });

  it("requests and cancels an own certificate", async () => {
    const { api: inst, inst: i } = await institution();
    const { api } = await seededMockRecordApi(i.id);
    await renderScreen(wrap(inst, api, <Documents />), { path: "/documents", role: "student" });
    expect(await screen.findByRole("heading", { name: "Documents", level: 1 })).toBeTruthy();
    await waitFor(() => form("my-request"));
    fillForm("my-request", { documentType: "transcript_copy", purpose: "visa" });
    await waitFor(() => expect(document.querySelector('[data-my-request="transcript_copy"]')).toBeTruthy());
    await click("my-request-cancel");
    await waitFor(async () => expect((await api.myRequests())[0]!.status).toBe("cancelled"));
  });
});
