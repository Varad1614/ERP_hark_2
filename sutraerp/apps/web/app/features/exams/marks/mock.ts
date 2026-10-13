/**
 * In-memory stand-in of the slice-6B API (mock mode and tests): one active session, one CS101
 * paper with two candidates, the sheet machine (planned → in_evaluation → evaluated → validated →
 * approved → finalised) and malpractice cases. Marks stay decimal strings; the preview percentage
 * is computed in integer hundredths (never floats).
 */
import { ApiProblem } from "@sutra/api-client";
import type { MarksApi } from "./api";
import type { Assessment, CaseCommand, MalpracticeCase, MarkRow, MarkRowList, MarkSheet, Moderation } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });

/** "61.5" → 6150 (hundredths). */
export function toHundredths(s: string): number {
  const [i = "0", f = ""] = s.trim().split(".");
  return Number(i || "0") * 100 + Number((f + "00").slice(0, 2));
}

export function fromHundredths(n: number): string {
  return `${Math.trunc(n / 100)}.${String(n % 100).padStart(2, "0")}`;
}

const SESSION = "00000000-0000-7000-8000-0000000b6b01";
const PAPER = "00000000-0000-7000-8000-0000000b6b02";

export function createMockMarksApi(): MarksApi {
  let n = 100;
  const id = () => `00000000-0000-7000-8000-${String(++n).padStart(12, "0")}`;
  const now = () => new Date().toISOString();
  let paperStatus = "conducted";
  let sheet: MarkSheet | null = null;
  const rows: MarkRow[] = [
    ["Aarav Desai", "S-0001", "EXR-000001"],
    ["Riya Sharma", "S-0002", "EXR-000002"],
  ].map(([name, num, reg], i) => ({
    id: `00000000-0000-7000-8000-0000000b6c0${i}`,
    paperId: PAPER,
    registrationId: `00000000-0000-7000-8000-0000000b6d0${i}`,
    registrationNumber: reg!,
    studentNumber: num!,
    displayName: name!,
    registrationStatus: "confirmed",
    paperStatus: "appeared",
    attemptKind: "regular",
    attemptNo: 1,
    internalMarks: null,
    internalMax: null,
    externalMarks: null,
    externalMax: "100.00",
    graceMarks: "0.00",
    absent: false,
    status: "pending",
    effective: true,
    preview: null,
    version: 1,
  }));
  const moderations: Moderation[] = [];
  const cases: MalpracticeCase[] = [];

  const preview = (r: MarkRow) => {
    if (r.absent || !r.externalMarks) return null;
    const internal = r.internalMarks && r.internalMax ? Math.round((toHundredths(r.internalMarks) * 4000) / toHundredths(r.internalMax)) : 0;
    const external = Math.round(((toHundredths(r.externalMarks) + toHundredths(r.graceMarks)) * 60) / 100);
    const pct = Math.min(internal + external, 10000);
    const bands: [number, string, string][] = [
      [9000, "O", "10.00"],
      [8000, "A+", "9.00"],
      [7000, "A", "8.00"],
      [6000, "B+", "7.00"],
      [5500, "B", "6.00"],
      [5000, "C", "5.00"],
      [4000, "P", "4.00"],
      [0, "F", "0.00"],
    ];
    const band = bands.find(([min]) => pct >= min) ?? bands[bands.length - 1]!;
    return { percent: fromHundredths(pct), grade: band[1], gradePoint: band[2], passed: pct >= 4000 };
  };
  const list = (): MarkRowList => ({ paperId: PAPER, paperCode: "CS101", paperStatus, sheet, items: rows.map((r) => ({ ...r, preview: preview(r) })) });
  const assessment = (): Assessment => ({
    paperId: PAPER,
    sessionId: SESSION,
    sessionCode: "ESE-2026-ODD",
    sessionName: "End-semester examinations",
    sessionStatus: "active",
    paperCode: "CS101",
    title: "Programming Fundamentals — end-semester",
    paperStatus,
    acceptsMarks: paperStatus === "conducted" || paperStatus === "evaluation",
    sheetId: sheet?.id ?? null,
    sheetStatus: sheet?.status ?? null,
    candidates: rows.length,
    entered: rows.filter((r) => r.status !== "pending").length,
    sections: ["A"],
  });
  const ensureSheet = (): MarkSheet => {
    if (!sheet) {
      sheet = { id: id(), sessionId: SESSION, paperId: PAPER, status: "planned", secondEvaluationRequired: false, differenceThreshold: "10.00", awaitingDecision: false, version: 1 };
      paperStatus = "evaluation";
    }
    return sheet;
  };
  const moveSheet = (version: number, from: string[], to: string, rowFrom: string[], rowTo: string): MarkSheet => {
    const s = ensureSheet();
    if (s.version !== version) throw conflict("stale version");
    if (!from.includes(s.status)) throw conflict(`the mark sheet is ${s.status}`);
    for (const r of rows) if (rowFrom.includes(r.status)) Object.assign(r, { status: rowTo, version: r.version + 1 });
    Object.assign(s, { status: to, version: s.version + 1 });
    return { ...s };
  };
  const findCase = (cid: string, version: number) => {
    const c = cases.find((x) => x.id === cid);
    if (!c) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
    if (c.version !== version) throw conflict("stale version");
    return c;
  };
  const moveCase = (c: MalpracticeCase, to: string, patch: Partial<MalpracticeCase> = {}) => {
    Object.assign(c, patch, { status: to, version: c.version + 1 });
    return { ...c };
  };
  const CASE_MOVES: Record<CaseCommand, [string[], string]> = {
    investigate: [["reported"], "under_investigation"],
    hearing: [["under_investigation"], "hearing"],
    appeal: [["decided"], "appealed"],
    reopen: [["decided", "upheld", "modified", "overturned", "closed"], "under_investigation"],
    close: [["decided", "upheld", "modified", "overturned"], "closed"],
    archive: [["closed"], "archived"],
  };

  return {
    myAssessments: async () => [assessment()],
    institutionPapers: async () => [assessment()],
    marks: async () => list(),
    enter: async (_paper, input) => {
      const s = ensureSheet();
      if (!["planned", "assigned", "in_evaluation"].includes(s.status)) throw conflict(`the mark sheet is ${s.status}`);
      for (const e of input) {
        const r = rows.find((x) => x.id === e.id);
        if (!r) continue;
        if (r.version !== e.version) throw conflict("stale version");
        if (e.externalMarks !== undefined && toHundredths(e.externalMarks) > 10000) {
          throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", detail: "externalMarks out of range" });
        }
        Object.assign(r, { externalMarks: e.absent ? null : e.externalMarks ? fromHundredths(toHundredths(e.externalMarks)) : r.externalMarks, absent: !!e.absent, status: "entered", version: r.version + 1 });
      }
      Object.assign(s, { status: "in_evaluation", version: s.version + 1 });
      return list();
    },
    openSheet: async () => {
      if (sheet) throw conflict("the paper's mark sheet is already open");
      return { ...ensureSheet() };
    },
    importInternal: async () => {
      for (const r of rows) Object.assign(r, { internalMarks: "34.50", internalMax: "40.00", version: r.version + 1 });
      return { processed: rows.length, skipped: 0, failed: [], missing: [] };
    },
    sheetCommand: async (_s, version, cmd) => {
      switch (cmd) {
        case "submit":
          if (rows.some((r) => r.status === "pending")) throw conflict("candidates have no marks");
          return { ...moveSheet(version, ["in_evaluation"], "evaluated", ["entered"], "submitted"), submittedAt: now() };
        case "verify":
          return { ...moveSheet(version, ["evaluated"], "validated", ["submitted"], "verified"), verifiedAt: now() };
        case "request-approval":
          return { ...moveSheet(version, ["validated"], "approved", ["verified", "moderated"], "approved"), approvedBy: "system:auto-approval" };
        case "finalise": {
          const s = moveSheet(version, ["approved"], "finalised", ["approved"], "finalised");
          if (rows.every((r) => r.status === "finalised")) paperStatus = "marks_finalised";
          return s;
        }
        default:
          throw conflict(`unknown command ${String(cmd)}`);
      }
    },
    decideSheet: async (_s, version, approve) => (approve ? moveSheet(version, ["validated"], "approved", ["verified", "moderated"], "approved") : { ...ensureSheet() }),
    withhold: async (mark, version, reason) => {
      const r = rows.find((x) => x.id === mark);
      if (!r || r.version !== version) throw conflict("stale version");
      Object.assign(r, { status: "withheld", version: r.version + 1 });
      return { ...r, reason };
    },
    release: async (mark, version) => {
      const r = rows.find((x) => x.id === mark);
      if (!r || r.version !== version || r.status !== "withheld") throw conflict("nothing to release");
      Object.assign(r, { status: "entered", version: r.version + 1 });
      return { ...r };
    },
    gradebook: async () => {
      const lines = rows.map((r) => {
        const p = preview(r);
        return { paperMarkId: r.id, registrationId: r.registrationId, studentNumber: r.studentNumber, displayName: r.displayName, attemptKind: r.attemptKind, status: r.status, absent: r.absent, internalMarks: r.internalMarks, externalMarks: r.externalMarks, graceMarks: r.graceMarks, percent: p?.percent ?? null, grade: p?.grade ?? null, gradePoint: p?.gradePoint ?? null, passed: p?.passed ?? false };
      });
      const graded = lines.filter((l) => l.percent);
      return {
        paperId: PAPER,
        paperCode: "CS101",
        title: "Programming Fundamentals — end-semester",
        credits: "4.00",
        passPercent: "40.00",
        internalWeight: "40.00",
        candidates: lines.length,
        graded: graded.length,
        passed: graded.filter((l) => l.passed).length,
        failed: graded.filter((l) => !l.passed).length,
        absent: lines.filter((l) => l.absent).length,
        averagePercent: graded.length ? fromHundredths(Math.round(graded.reduce((a, l) => a + toHundredths(l.percent!), 0) / graded.length)) : null,
        distribution: [],
        lines,
      };
    },
    moderations: async () => moderations.map((m) => ({ ...m })),
    requestModeration: async (_paper, kind, value, reason) => {
      const s = ensureSheet();
      if (s.status !== "validated") throw conflict(`the mark sheet is ${s.status}`);
      const m: Moderation = { id: id(), sessionId: SESSION, paperId: PAPER, markSheetId: s.id, kind, value, reason, status: "requested", report: [], appliedCount: 0, createdAt: now(), version: 1 };
      moderations.unshift(m);
      Object.assign(s, { status: "moderation", version: s.version + 1 });
      return { ...m };
    },
    moderationCommand: async (mid, version, cmd) => {
      const m = moderations.find((x) => x.id === mid);
      if (!m || m.version !== version) throw conflict("stale version");
      const s = ensureSheet();
      if (cmd === "validate") Object.assign(m, { status: "approved", version: m.version + 1 });
      else if (cmd === "withdraw") {
        Object.assign(m, { status: "withdrawn", version: m.version + 1 });
        Object.assign(s, { status: "validated", version: s.version + 1 });
      } else {
        let applied = 0;
        for (const r of rows.filter((x) => x.status === "verified")) {
          applied += 1;
          Object.assign(r, { graceMarks: kind(m) === "grace" ? fromHundredths(toHundredths(r.graceMarks) + toHundredths(m.value)) : r.graceMarks, status: "moderated", version: r.version + 1 });
        }
        Object.assign(m, { status: "applied", appliedCount: applied, version: m.version + 1 });
        Object.assign(s, { status: "validated", version: s.version + 1 });
      }
      return { ...m };
    },
    cases: async (session) => cases.filter((c) => c.sessionId === session).map((c) => ({ ...c })),
    reportCase: async (session, input) => {
      if (cases.some((c) => c.registrationId === input.registrationId && (c.paperId ?? null) === (input.paperId ?? null) && !["closed", "archived"].includes(c.status))) {
        throw conflict("a malpractice case is already open for this candidate and paper");
      }
      const c: MalpracticeCase = { id: id(), caseNumber: `MAL-${String(cases.length + 1).padStart(6, "0")}`, sessionId: session, registrationId: input.registrationId, paperId: input.paperId ?? null, studentId: id(), allegation: input.allegation, evidence: input.evidence ?? null, status: "reported", createdAt: now(), decisions: [], version: 1 };
      cases.unshift(c);
      return { ...c };
    },
    caseCommand: async (cid, version, cmd, reason) => {
      const c = findCase(cid, version);
      const [from, to] = CASE_MOVES[cmd];
      if (!from.includes(c.status)) throw conflict(`malpractice case ${c.caseNumber} is ${c.status}`);
      return moveCase(c, to, { statusReason: reason ?? null });
    },
    decideCase: async (cid, version, penalty, reason, marksDeduction) => {
      const c = findCase(cid, version);
      if (!["under_investigation", "hearing"].includes(c.status)) throw conflict(`malpractice case ${c.caseNumber} is ${c.status}`);
      return moveCase(c, "decided", { penalty, marksDeduction: marksDeduction ?? null, decisions: [...c.decisions, { id: id(), kind: "decision", outcome: "decided", penalty, reason, createdAt: now() }] });
    },
    reviewCase: async (cid, version, outcome, reason, penalty) => {
      const c = findCase(cid, version);
      if (c.status !== "appealed") throw conflict(`malpractice case ${c.caseNumber} is ${c.status}`);
      const to = outcome === "uphold" ? "upheld" : outcome === "modify" ? "modified" : "overturned";
      const newPenalty = outcome === "modify" ? (penalty ?? c.penalty) : outcome === "overturn" ? "none" : c.penalty;
      return moveCase(c, to, { penalty: newPenalty, decisions: [...c.decisions, { id: id(), kind: "review", outcome: to, penalty: newPenalty, reason, createdAt: now() }] });
    },
    approveCase: async (cid, version) => {
      const c = findCase(cid, version);
      return moveCase(c, c.status, { pending: null });
    },
  };
}

function kind(m: Moderation): string {
  return m.kind;
}
