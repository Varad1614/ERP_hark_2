/**
 * In-memory stand-in of the slice-6A API (mock mode and tests). It follows the server's main
 * rules: policy draft → approved (auto) → active superseding the previous version; session
 * draft → approved (auto) → scheduled → active; a timetable must be approved before
 * publication; one live registration per student and session; hall tickets are withheld while
 * dues are outstanding.
 */
import { ApiProblem } from "@sutra/api-client";
import type { ExamSetupApi } from "./api";
import type { ExamPolicy, ExamRegistration, ExamSchedule, ExamSession, ExamVenue, HallTicket } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const invalid = (detail: string) => new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation", detail });

const POLICY_NEXT: Record<string, Record<string, string>> = { submit: { draft: "approved" }, activate: { approved: "active", scheduled: "active" }, retire: { active: "retired", superseded: "retired", approved: "retired" } };
const SESSION_NEXT: Record<string, Record<string, string>> = { submit: { draft: "approved" }, schedule: { approved: "scheduled" }, activate: { scheduled: "active" }, suspend: { active: "suspended", reopened: "suspended" }, reopen: { suspended: "reopened" } };
const SCHEDULE_NEXT: Record<string, Record<string, string>> = { submit: { draft: "approved" }, publish: { approved: "published" }, revise: { published: "revised" }, withdraw: { published: "cancelled" } };

export function createMockExamSetupApi(): ExamSetupApi {
  let n = 0;
  const id = () => `00000000-0000-7000-8000-${String(++n).padStart(12, "0")}`;
  const policies: ExamPolicy[] = [];
  const sessions: ExamSession[] = [];
  const schedules: ExamSchedule[] = [];
  const venues: ExamVenue[] = [];
  const regs: ExamRegistration[] = [];
  const tickets: HallTicket[] = [];
  let duesOutstanding = true;
  const find = <T extends { id: string; version: number }>(list: T[], key: string, version: number): T => {
    const x = list.find((v) => v.id === key);
    if (!x) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
    if (x.version !== version) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });
    return x;
  };
  const move = <T extends { status: string; version: number }>(x: T, table: Record<string, Record<string, string>>, cmd: string, label: string): T => {
    const to = table[cmd]?.[x.status];
    if (!to) throw conflict(`${label} is ${x.status}; it cannot ${cmd}`);
    x.status = to;
    x.version += 1;
    return { ...x };
  };
  const studentSession = () => sessions.find((s) => ["active", "reopened"].includes(s.status));
  return {
    policies: async () => policies.map((p) => ({ ...p })),
    createPolicy: async (inst, input) => {
      const code = String(input.code ?? "");
      if (!/^[A-Z0-9][A-Z0-9_-]{1,31}$/.test(code)) throw invalid("code must be 2–32 characters A–Z, 0–9, '-' or '_'");
      const prev = policies.filter((p) => p.code === code);
      if (prev.some((p) => p.status === "draft")) throw conflict(`policy ${code} already has an open version`);
      const p: ExamPolicy = {
        id: id(),
        institutionId: inst,
        code,
        name: String(input.name ?? code),
        versionNo: prev.length + 1,
        supersedesId: prev.at(-1)?.id ?? null,
        status: "draft",
        requireDuesCleared: Boolean(input.requireDuesCleared ?? true),
        lateRegistrationAllowed: true,
        minAttendancePercent: (input.minAttendancePercent as string | undefined) ?? null,
        maxAttempts: 4,
        invigilatorRatio: 30,
        version: 1,
      };
      policies.push(p);
      return { ...p };
    },
    policyCommand: async (pid, v, cmd, reason) => {
      const p = find(policies, pid, v);
      if (cmd === "retire" && !reason) throw invalid("a reason is required");
      if (cmd === "activate") for (const o of policies) if (o.code === p.code && o.status === "active" && o.id !== p.id) o.status = "superseded";
      return move(p, POLICY_NEXT, cmd, `policy ${p.code} v${p.versionNo}`);
    },
    sessions: async () => sessions.map((s) => ({ ...s, papers: [...s.papers] })),
    gradingSchemes: async () => [{ id: "00000000-0000-7000-8000-00000000a001", code: "TEN-POINT", name: "Ten-point scale", status: "active" }],
    createSession: async (inst, input) => {
      const s: ExamSession = {
        id: id(),
        institutionId: inst,
        academicTermId: String(input.academicTermId ?? ""),
        code: String(input.code ?? ""),
        name: String(input.name ?? ""),
        kind: "regular",
        status: "draft",
        policyId: policies.find((p) => p.status === "active")?.id ?? null,
        gradingSchemeId: (input.gradingSchemeId as string | undefined) ?? null,
        registrationOpensOn: (input.registrationOpensOn as string | undefined) ?? null,
        registrationClosesOn: (input.registrationClosesOn as string | undefined) ?? null,
        revisions: 0,
        papers: [],
        version: 1,
      };
      if (!s.code || !s.name) throw invalid("code and name are required");
      sessions.push(s);
      return { ...s };
    },
    sessionCommand: async (sid, v, cmd, reason) => {
      const s = find(sessions, sid, v);
      if (cmd === "suspend" && !reason) throw invalid("a reason is required");
      if (cmd === "activate" && s.papers.length === 0) throw conflict("the session has no papers; add papers before activating it");
      return move(s, SESSION_NEXT, cmd, `session ${s.code}`);
    },
    addPaper: async (sid, offeringId) => {
      const s = sessions.find((x) => x.id === sid);
      if (!s) throw conflict("unknown session");
      if (s.papers.some((p) => p.offeringId === offeringId)) throw conflict("a paper already examines this offering");
      const p = { id: id(), sessionId: sid, offeringId, paperCode: `P${s.papers.length + 1}`, title: "Paper", credits: "4.00", mode: "theory", maxMarks: "100.00", passPercent: "40.00", internalWeight: "0.00", durationMinutes: 180, status: "draft", version: 1 };
      s.papers.push(p);
      return p;
    },
    generatePapers: async (sid) => {
      const s = sessions.find((x) => x.id === sid);
      if (!s) throw conflict("unknown session");
      const before = s.papers.length;
      if (before === 0) s.papers.push({ id: id(), sessionId: sid, offeringId: id(), paperCode: "CS101", title: "Programming", credits: "4.00", mode: "theory", maxMarks: "100.00", passPercent: "40.00", internalWeight: "40.00", durationMinutes: 180, status: "draft", version: 1 });
      return { processed: s.papers.length - before, skipped: before, failed: [] };
    },
    schedules: async (sid) => schedules.filter((s) => s.sessionId === sid).map((s) => ({ ...s })),
    createSchedule: async (sid, name) => {
      if (schedules.some((s) => s.sessionId === sid && ["draft", "approved", "published"].includes(s.status))) throw conflict("the session already has a live schedule");
      const s: ExamSchedule = { id: id(), sessionId: sid, name, versionNo: schedules.filter((x) => x.sessionId === sid).length + 1, status: "draft", sittings: [], conflicts: [], version: 1 };
      schedules.push(s);
      return { ...s };
    },
    scheduleCommand: async (sch, v, cmd, reason) => {
      const s = find(schedules, sch, v);
      if ((cmd === "revise" || cmd === "withdraw") && !reason) throw invalid("a reason is required");
      if (cmd === "submit" && s.sittings.length === 0) throw conflict("the schedule has no sittings yet");
      const out = move(s, SCHEDULE_NEXT, cmd, `schedule ${s.name}`);
      if (cmd === "revise") {
        const next: ExamSchedule = { ...s, id: id(), versionNo: s.versionNo + 1, supersedesId: s.id, status: "draft", version: 1, sittings: s.sittings.map((x) => ({ ...x, id: id() })) };
        schedules.push(next);
        return { ...next };
      }
      return out;
    },
    addSitting: async (sch, input) => {
      const s = schedules.find((x) => x.id === sch);
      if (!s || s.status !== "draft") throw conflict("revise the schedule to change sittings");
      const session = sessions.find((x) => x.id === s.sessionId);
      const paper = session?.papers.find((p) => p.id === input.paperId);
      if (!paper) throw invalid("unknown examination paper");
      const x = { id: id(), scheduleId: sch, sessionId: s.sessionId, paperId: paper.id, paperCode: paper.paperCode, title: paper.title, mode: paper.mode, sitsOn: input.sitsOn, startsAt: input.startsAt, durationMinutes: 180, status: "planned", venues: [], version: 1 };
      s.sittings.push(x);
      return x;
    },
    venues: async () => venues.map((v) => ({ ...v })),
    createVenue: async (_inst, input) => {
      if (venues.some((v) => v.code === input.code)) throw conflict(`venue ${input.code} exists`);
      const v: ExamVenue = { id: id(), code: input.code, name: input.name, capacity: input.capacity, status: "active", version: 1 };
      venues.push(v);
      return v;
    },
    allocate: async (sitting, venueId) => {
      const x = schedules.flatMap((s) => s.sittings).find((s) => s.id === sitting);
      const v = venues.find((y) => y.id === venueId);
      if (!x || !v) throw invalid("unknown sitting or venue");
      x.venues.push({ id: id(), sittingId: sitting, venueId, venueCode: v.code, venueName: v.name, capacity: v.capacity, seats: v.capacity, status: "allocated" });
      return { items: x.venues };
    },
    generateSeating: async (sitting) => {
      const x = schedules.flatMap((s) => s.sittings).find((s) => s.id === sitting);
      if (!x?.venues.length) throw conflict("allocate at least one venue to the sitting before seating candidates");
      const candidates = regs.filter((r) => r.sessionId === x.sessionId && ["confirmed", "locked"].includes(r.status)).length;
      if (candidates > x.venues.reduce((t, v) => t + v.seats, 0)) throw conflict("not enough allocated seats");
      return { id: id(), version: 1, candidates };
    },
    registrations: async (sid) => regs.filter((r) => r.sessionId === sid).map((r) => ({ ...r })),
    bulkRegister: async (sid) => {
      const before = regs.length;
      for (const [studentId, name] of [["00000000-0000-7000-8000-0000000005a1", "Aarav Desai"], ["00000000-0000-7000-8000-0000000005a2", "Diya Rao"]] as const)
        if (!regs.some((r) => r.sessionId === sid && r.studentId === studentId && r.status !== "withdrawn")) regs.push(newReg(sid, studentId, name, "bulk"));
      return { processed: regs.length - before, skipped: 2 - (regs.length - before), failed: [] };
    },
    register: async (sid, studentId) => {
      if (regs.some((r) => r.sessionId === sid && r.studentId === studentId && !["withdrawn", "rejected"].includes(r.status))) throw conflict("the candidate already holds a registration in this session");
      const r = newReg(sid, studentId, "Candidate", "administrative");
      regs.push(r);
      return { ...r };
    },
    registrationCommand: async (rid, v, cmd, reason) => {
      const r = find(regs, rid, v);
      if (cmd !== "confirm" && !reason) throw invalid("a reason is required");
      const next = { confirm: { amended: "confirmed" }, withdraw: { confirmed: "withdrawn", locked: "withdrawn" }, reinstate: { withdrawn: "confirmed" } } as Record<string, Record<string, string>>;
      const out = move(r, next, cmd, `registration ${r.registrationNumber}`);
      if (cmd === "withdraw") for (const t of tickets) if (t.registrationId === rid && t.status === "active") t.status = "withdrawn";
      return out;
    },
    hallTickets: async (sid) => tickets.filter((t) => t.sessionId === sid).map((t) => ({ ...t })),
    issueHallTickets: async (sid) => {
      let processed = 0;
      let skipped = 0;
      for (const r of regs.filter((x) => x.sessionId === sid && ["confirmed", "locked"].includes(x.status))) {
        if (tickets.some((t) => t.registrationId === r.id && ["active", "withheld"].includes(t.status))) {
          skipped += 1;
          continue;
        }
        const withheld = duesOutstanding && r.studentId.endsWith("5a1");
        const t: HallTicket = { id: id(), sessionId: sid, registrationId: r.id, studentId: r.studentId, number: `HT-${String(tickets.length + 1).padStart(6, "0")}`, status: withheld ? "withheld" : "active", withheldReason: withheld ? "outstanding dues: ₹1,000.00" : null, version: 1 };
        tickets.push(t);
        r.hallTicket = { id: t.id, number: t.number, status: t.status };
        processed += 1;
      }
      return { processed, skipped, failed: [] };
    },
    ticketCommand: async (tid, v, cmd, reason) => {
      const t = find(tickets, tid, v);
      if (cmd !== "release" && !reason) throw invalid("a reason is required");
      if (cmd === "release") {
        if (t.status !== "withheld") throw conflict(`hall ticket ${t.number} is ${t.status}`);
        duesOutstanding = false;
        t.withheldReason = null;
      }
      const next = { withhold: { active: "withheld" }, release: { withheld: "active" }, withdraw: { active: "withdrawn", withheld: "withdrawn" }, reissue: { active: "reissued" } } as Record<string, Record<string, string>>;
      const out = move(t, next, cmd, `hall ticket ${t.number}`);
      if (cmd === "reissue") {
        const nt: HallTicket = { ...t, id: id(), number: `HT-${String(tickets.length + 1).padStart(6, "0")}`, status: "active", replacesId: t.id, version: 1 };
        tickets.push(nt);
        return { ...nt };
      }
      return out;
    },
    finalisePopulation: async (sid) => {
      let processed = 0;
      for (const r of regs.filter((x) => x.sessionId === sid && ["confirmed", "amended"].includes(x.status))) {
        r.status = "locked";
        r.version += 1;
        processed += 1;
      }
      return { processed, skipped: 0, failed: [] };
    },
    myOptions: async () => {
      const s = studentSession();
      if (!s) return [];
      return [{ sessionId: s.id, code: s.code, name: s.name, registrationOpensOn: s.registrationOpensOn, registrationClosesOn: s.registrationClosesOn, window: "open", registered: regs.some((r) => r.sessionId === s.id && r.studentId.endsWith("5a1") && r.status !== "withdrawn"), papers: s.papers.map((p) => ({ paperId: p.id, paperCode: p.paperCode, title: p.title, credits: p.credits, covered: true })) }];
    },
    myRegistrations: async () => regs.filter((r) => r.studentId.endsWith("5a1")).map((r) => ({ ...r })),
    registerMe: async (sessionId) => {
      const s = sessions.find((x) => x.id === sessionId);
      if (!s || !["active", "reopened"].includes(s.status)) throw conflict("the session is not open for registration");
      if (regs.some((r) => r.sessionId === sessionId && r.studentId.endsWith("5a1") && r.status !== "withdrawn")) throw conflict("you are already registered for this session");
      const r = newReg(sessionId, "00000000-0000-7000-8000-0000000005a1", "Aarav Desai", "self_service");
      r.papers = s.papers.map((p) => ({ registrationPaperId: id(), paperId: p.id, paperCode: p.paperCode, title: p.title, attemptKind: "regular", attemptNo: 1, status: "registered" }));
      regs.push(r);
      return { ...r };
    },
    myHallTickets: async () =>
      tickets
        .filter((t) => t.studentId.endsWith("5a1") && ["active", "withheld"].includes(t.status))
        .map((t) => ({ registrationNumber: regs.find((r) => r.id === t.registrationId)?.registrationNumber ?? "", displayName: "Aarav Desai", studentNumber: "S-0001", ticket: { ...t } })),
    myTimetable: async () => {
      const mine = regs.filter((r) => r.studentId.endsWith("5a1") && ["confirmed", "locked"].includes(r.status));
      return schedules
        .filter((s) => s.status === "published" && mine.some((r) => r.sessionId === s.sessionId))
        .flatMap((s) => s.sittings.map((x) => ({ sessionId: s.sessionId, registrationId: mine[0]!.id, sittingId: x.id, paperId: x.paperId, paperCode: x.paperCode, title: x.title, sitsOn: x.sitsOn, startsAt: x.startsAt, durationMinutes: x.durationMinutes, status: x.status, venueCode: x.venues[0]?.venueCode ?? null, seatNo: x.venues[0] ? `${x.venues[0].venueCode}-001` : null })));
    },
  };

  function newReg(sessionId: string, studentId: string, displayName: string, channel: string): ExamRegistration {
    return { id: id(), registrationNumber: `EXR-${String(regs.length + 1).padStart(6, "0")}`, sessionId, studentId, studentNumber: `S-${String(regs.length + 1).padStart(4, "0")}`, displayName, channel, status: "confirmed", papers: [], hallTicket: null, arrangements: [], lateFee: null, version: 1 };
  }
}
