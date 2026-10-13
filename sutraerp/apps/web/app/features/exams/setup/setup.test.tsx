/** Slice 6A: path table and the in-memory API's policy / session / registration / hall-ticket flow. */
import { describe, expect, it } from "vitest";
import { PATHS } from "./api";
import { createMockExamSetupApi } from "./mock";

describe("6A paths", () => {
  it("keeps every path under the exams prefix", () => {
    for (const p of Object.values(PATHS)) expect(p.startsWith("/api/v1/exams/")).toBe(true);
  });
});

describe("6A mock API", () => {
  it("activates a policy version, superseding the previous one", async () => {
    const api = createMockExamSetupApi();
    await expect(api.createPolicy("i", { code: "bad code" })).rejects.toBeTruthy();
    let p1 = await api.createPolicy("i", { code: "EXAM-POL" });
    await expect(api.policyCommand(p1.id, p1.version, "activate")).rejects.toBeTruthy();
    p1 = await api.policyCommand(p1.id, p1.version, "submit");
    p1 = await api.policyCommand(p1.id, p1.version, "activate");
    expect(p1.status).toBe("active");
    let p2 = await api.createPolicy("i", { code: "EXAM-POL" });
    expect(p2.versionNo).toBe(2);
    p2 = await api.policyCommand(p2.id, p2.version, "submit");
    await api.policyCommand(p2.id, p2.version, "activate");
    const all = await api.policies("i");
    expect(all.map((p) => p.status)).toEqual(["superseded", "active"]);
  });

  it("runs a session to registration, a withheld hall ticket and its release", async () => {
    const api = createMockExamSetupApi();
    let s = await api.createSession("i", { code: "ESE", name: "End semester", academicTermId: "t" });
    s = await api.sessionCommand(s.id, s.version, "submit");
    s = await api.sessionCommand(s.id, s.version, "schedule");
    await expect(api.sessionCommand(s.id, s.version, "activate")).rejects.toBeTruthy();
    expect((await api.generatePapers(s.id)).processed).toBe(1);
    s = await api.sessionCommand(s.id, s.version, "activate");
    expect(s.status).toBe("active");
    const options = await api.myOptions();
    expect(options[0]?.window).toBe("open");
    const mine = await api.registerMe(s.id);
    expect(mine.status).toBe("confirmed");
    await expect(api.registerMe(s.id)).rejects.toBeTruthy();
    const bulk = await api.bulkRegister(s.id);
    expect([bulk.processed, bulk.skipped]).toEqual([1, 1]);
    expect((await api.issueHallTickets(s.id)).processed).toBe(2);
    const t = (await api.myHallTickets())[0]!.ticket;
    expect(t.status).toBe("withheld");
    const released = await api.ticketCommand(t.id, t.version, "release");
    expect(released.status).toBe("active");
    expect((await api.finalisePopulation(s.id)).processed).toBe(2);
  });
});
