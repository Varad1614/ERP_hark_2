/**
 * DEF-I-007 regression: the exam office can conduct a sitting in the app — publish the seating
 * plan, open / start the conduct, record attendance (paper conducted, so marks entry can start),
 * assign invigilators and record exceptions — on the existing `exam-scheduling` endpoints.
 */
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Institution } from "../../institution/types";
import type { ExamSetupApi } from "../setup/api";
import type { ExamRegistration } from "../setup/types";
import { createHttpExamConductApi, PATHS } from "./api";
import { createMockExamConductApi } from "./mock";

const INST = { id: "inst-6c", name: "Test College", timezone: "Asia/Kolkata" } as unknown as Institution;

beforeEach(() => {
  vi.resetModules();
  window.sessionStorage.clear();
});

describe("conduct paths", () => {
  it("keeps every path under the exams prefix", () => {
    for (const p of Object.values(PATHS)) expect(p.startsWith("/api/v1/exams/")).toBe(true);
  });

  it("sends If-Match on commands, Idempotency-Key on creates, and maps a missing record to null", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const fetchImpl = async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      if (url.endsWith("/conduct") && init.method === "GET")
        return new Response(JSON.stringify({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found", detail: "conduct record not found" }), { status: 404 });
      return new Response(JSON.stringify({ id: "c1", status: "completed", version: 4, attendance: [] }), { status: 200 });
    };
    const api = createHttpExamConductApi(fetchImpl, "");
    expect(await api.conduct("s1")).toBeNull();
    await api.completeConduct("c1", 3, [{ registrationId: "r1", outcome: "present" }]);
    const complete = calls[1]!;
    expect(complete.url).toBe(PATHS.cComplete.replace("{id}", "c1"));
    expect((complete.init.headers as Record<string, string>)["If-Match"]).toBe('"3"');
    expect(JSON.parse(String(complete.init.body))).toEqual({ attendance: [{ registrationId: "r1", outcome: "present" }] });
    await api.openConduct("s1");
    expect((calls[2]!.init.headers as Record<string, string>)["Idempotency-Key"]).toBeTruthy();
    await api.publishSeating("p1", 1);
    expect(calls[3]!.url).toBe(PATHS.spPublish.replace("{id}", "p1"));
  });
});

describe("conduct mock API", () => {
  it("runs a sitting from seating publication to a completed conduct", async () => {
    const api = createMockExamConductApi();
    const plan = await api.generateSeating("sit-1");
    const published = await api.publishSeating(plan.id, plan.version);
    expect(published.status).toBe("published");
    await expect(api.generateSeating("sit-1")).rejects.toBeTruthy();
    let c = await api.openConduct("sit-1");
    await expect(api.openConduct("sit-1")).rejects.toBeTruthy();
    await expect(api.completeConduct(c.id, c.version, [])).rejects.toBeTruthy();
    c = await api.conductCommand(c.id, c.version, "start");
    c = await api.completeConduct(c.id, c.version, [
      { registrationId: "r1", outcome: "present" },
      { registrationId: "r2", outcome: "absent" },
    ]);
    expect([c.status, c.presentCount, c.absentCount]).toEqual(["completed", 1, 1]);
  });

  it("assigns, confirms and replaces duties; walks an exception to closure", async () => {
    const api = createMockExamConductApi();
    await expect(api.assignDuty("sit-1", { subjectRef: " " })).rejects.toBeTruthy();
    const d = await api.assignDuty("sit-1", { subjectRef: "dev|a@x", name: "A" });
    expect((await api.invigilation("sit-1")).shortage).toBe(0);
    const confirmed = await api.dutyCommand(d.id, d.version, "confirm");
    await expect(api.replaceDuty(d.id, confirmed.version, { subjectRef: "dev|b@x", reason: "" })).rejects.toBeTruthy();
    const r = await api.replaceDuty(d.id, confirmed.version, { subjectRef: "dev|b@x", reason: "Unwell" });
    expect(r.replacesId).toBe(d.id);
    let x = await api.raiseException("ses-1", { category: "conduct", description: "Late start" });
    expect(x.number).toBe("EXC-000001");
    await expect(api.resolveException(x.id, x.version, "n/a")).rejects.toBeTruthy();
    x = await api.classifyException(x.id, x.version, { classification: "Late start", severity: "low" });
    x = await api.resolveException(x.id, x.version, "Extra 15 minutes granted");
    x = await api.exceptionCommand(x.id, x.version, "close");
    expect(x.status).toBe("closed");
  });
});

describe("candidates and attendance", () => {
  it("lists only effective registrations holding the paper and maps the dialog values", async () => {
    const { attendanceField, attendanceFrom, sittingCandidates } = await import("./index");
    const reg = (id: string, status: string, paperId: string, paperStatus = "registered") =>
      ({ id, studentNumber: `S-${id}`, status, papers: [{ paperId, status: paperStatus }] }) as unknown as ExamRegistration;
    const regs = [reg("1", "confirmed", "p1"), reg("2", "withdrawn", "p1"), reg("3", "confirmed", "p2"), reg("4", "locked", "p1", "withdrawn"), reg("5", "locked", "p1")];
    const c = sittingCandidates(regs, "p1");
    expect(c.map((r) => r.id)).toEqual(["1", "5"]);
    expect(attendanceFrom(c, { [attendanceField(c[1]!)]: "absent" })).toEqual([
      { registrationId: "1", outcome: "present" },
      { registrationId: "5", outcome: "absent" },
    ]);
  });
});

/** A 6A mock with an active session, a published timetable with one CS101 sitting and one candidate. */
async function seededSetup(): Promise<{ api: ExamSetupApi; sittingId: string; registrationId: string }> {
  const { createMockExamSetupApi } = await import("../setup/mock");
  const base = createMockExamSetupApi();
  let s = await base.createSession(INST.id, { code: "ESE", name: "End semester", academicTermId: "t" });
  s = await base.sessionCommand(s.id, s.version, "submit");
  s = await base.sessionCommand(s.id, s.version, "schedule");
  await base.generatePapers(s.id);
  s = await base.sessionCommand(s.id, s.version, "activate");
  const paper = (await base.sessions(INST.id)).find((x) => x.id === s.id)!.papers[0]!;
  let sch = await base.createSchedule(s.id, "ESE timetable");
  const sitting = await base.addSitting(sch.id, { paperId: paper.id, sitsOn: "2030-01-10", startsAt: "09:30" });
  sch = await base.scheduleCommand(sch.id, sch.version, "submit");
  await base.scheduleCommand(sch.id, sch.version, "publish");
  await base.bulkRegister(s.id);
  const [first] = await base.registrations(s.id);
  const reg: ExamRegistration = { ...first!, papers: [{ registrationPaperId: "rp1", paperId: paper.id, paperCode: paper.paperCode, title: paper.title, attemptKind: "regular", attemptNo: 1, status: "registered" }] };
  return { api: { ...base, registrations: async () => [reg] }, sittingId: sitting.id, registrationId: reg.id };
}

async function clickAction(id: string) {
  const btn = await waitFor(() => {
    const b = document.querySelector<HTMLButtonElement>(`[data-action="${id}"]`);
    if (!b || b.disabled) throw new Error(`action ${id} not ready`);
    return b;
  });
  fireEvent.click(btn);
}

function setField(form: Element, name: string, value: string) {
  const el = form.querySelector<HTMLInputElement | HTMLSelectElement>(`[name="${name}"]`);
  if (!el) throw new Error(`no field ${name}`);
  fireEvent.change(el, { target: { value } });
}

describe("Admin → Examinations → Conduct", () => {
  it("publishes seating, conducts the sitting with attendance and records an exception through the screen", async () => {
    const { renderScreen } = await import("../../../test/harness");
    const { ConductAdminTab, ExamConductApiProvider } = await import("./index");
    const { ExamSetupApiProvider } = await import("../setup/hooks");
    const setup = await seededSetup();
    const conduct = createMockExamConductApi();
    await conduct.generateSeating(setup.sittingId);
    await renderScreen(
      <ExamSetupApiProvider api={setup.api}>
        <ExamConductApiProvider api={conduct}>
          <ConductAdminTab institution={INST} />
        </ExamConductApiProvider>
      </ExamSetupApiProvider>,
      { path: "/examinations/:tab?", entry: "/examinations/conduct" },
    );
    expect((await screen.findAllByText("Sittings")).length).toBeGreaterThan(0);
    await waitFor(() => expect(document.querySelector('[data-conduct-sitting="CS101"]')).toBeTruthy());

    await clickAction("seating-publish");
    await waitFor(async () => expect((await conduct.seating(setup.sittingId))?.status).toBe("published"));
    await clickAction("conduct-open");
    await waitFor(async () => expect((await conduct.conduct(setup.sittingId))?.status).toBe("scheduled"));
    await clickAction("conduct-start");
    await waitFor(async () => expect((await conduct.conduct(setup.sittingId))?.status).toBe("in_progress"));
    await clickAction("conduct-complete");
    const form = await waitFor(() => {
      const f = document.querySelector('[data-form="conduct-complete"]');
      if (!f) throw new Error("attendance dialog not open");
      return f;
    });
    const select = form.querySelector<HTMLSelectElement>('select[name^="att-"]')!;
    expect(select.value).toBe("present");
    setField(form, select.name, "absent");
    fireEvent.click(form.querySelector('button[type="submit"]')!);
    await waitFor(async () => {
      const c = await conduct.conduct(setup.sittingId);
      expect(c?.status).toBe("completed");
      expect(c?.attendance).toEqual([expect.objectContaining({ registrationId: setup.registrationId, outcome: "absent" })]);
    });
    await waitFor(() => expect(document.querySelector('[data-conduct-status="completed"]')).toBeTruthy());

    fireEvent.click(document.querySelector("[data-raise-exception]")!);
    const raise = await waitFor(() => {
      const f = document.querySelector('[data-form="exception-raise"]');
      if (!f) throw new Error("exception dialog not open");
      return f;
    });
    setField(raise, "category", "conduct");
    setField(raise, "description", "Question paper packet opened late");
    fireEvent.click(raise.querySelector('button[type="submit"]')!);
    await waitFor(() => expect(document.querySelector('[data-exception="EXC-000001"]')).toBeTruthy());
  });
});
