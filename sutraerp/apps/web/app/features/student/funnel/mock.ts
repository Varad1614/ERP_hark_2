/**
 * In-memory `FunnelApi` for mock mode and screen tests. Lightweight on purpose: the server's
 * funnel maths (golden-tested against Everything) is authoritative.
 */
import type { FunnelApi } from "./api";
import type { Enquiry, FunnelRow, FunnelSettings, FunnelStage } from "./types";

const DEFAULT_STAGES: FunnelStage[] = [
  { id: "enquiry", label: "Enquiry", probability: 0.1 },
  { id: "counselling", label: "Counselling", probability: 0.25 },
  { id: "campus_visit", label: "Campus visit", probability: 0.4 },
  { id: "applied", label: "Applied", probability: 0.6 },
  { id: "offered", label: "Offered", probability: 0.8 },
];
const TERMINAL: FunnelStage[] = [
  { id: "admitted", label: "Admitted", probability: 1 },
  { id: "lost", label: "Lost", probability: 0 },
];
const DAY = 86_400_000;

export function createMockFunnelApi(): FunnelApi {
  const rows: Enquiry[] = [];
  let settings: FunnelSettings = { stages: [...DEFAULT_STAGES, ...TERMINAL], customised: false, staleDays: 14, targets: [], version: 0 };
  let seq = 0;
  const open = () => settings.stages.filter((s) => s.id !== "admitted" && s.id !== "lost");
  const finish = (e: Enquiry): Enquiry => {
    const days = Math.max(0, Math.floor((Date.now() - Date.parse(e.stageEnteredAt)) / DAY));
    const stage = settings.stages.find((s) => s.id === e.stage);
    const terminal = e.stage === "admitted" || e.stage === "lost";
    return { ...e, stageLabel: stage?.label ?? e.stage, daysInStage: days, stalled: !terminal && settings.staleDays > 0 && days >= settings.staleDays, probability: e.stage === "admitted" ? 1 : e.stage === "lost" ? 0 : (e.confidence ?? stage?.probability ?? 0) };
  };
  const find = (id: string) => {
    const e = rows.find((r) => r.id === id);
    if (!e) throw new Error("enquiry not found");
    return e;
  };
  const move = (e: Enquiry, stage: string, reason?: string | null) => {
    const at = new Date().toISOString();
    e.stage = stage;
    e.stageHistory = [...e.stageHistory, { stage, at }];
    e.stageEnteredAt = at;
    const closed = stage === "admitted" || stage === "lost";
    e.closedAt = closed ? at : null;
    e.closeReason = closed ? (reason ?? null) : null;
  };
  return {
    async enquiries(_inst, f = {}) {
      const items = rows
        .map(finish)
        .filter((e) => !f.stage || e.stage === f.stage)
        .filter((e) => !f.open || (e.stage !== "admitted" && e.stage !== "lost"))
        .filter((e) => !f.q || `${e.displayName} ${e.enquiryNumber} ${e.phone ?? ""} ${e.email ?? ""}`.toLowerCase().includes(f.q.toLowerCase()));
      return { items, stages: settings.stages, staleDays: settings.staleDays };
    },
    async createEnquiry(inst, input) {
      if (!input.givenName.trim()) throw new Error("givenName is required");
      const now = new Date().toISOString();
      const stage = input.stage || open()[0]!.id;
      seq += 1;
      const e: Enquiry = {
        id: `enq-${seq}`,
        institutionId: inst,
        enquiryNumber: `ENQ-${now.slice(0, 4)}-${String(seq).padStart(5, "0")}`,
        programmeId: input.programmeId ?? null,
        intake: input.intake ?? null,
        givenName: input.givenName.trim(),
        familyName: input.familyName ?? null,
        displayName: [input.givenName.trim(), input.familyName?.trim()].filter(Boolean).join(" "),
        email: input.email ?? null,
        phone: input.phone ?? null,
        guardianName: input.guardianName ?? null,
        guardianPhone: input.guardianPhone ?? null,
        city: input.city ?? null,
        source: input.source ?? "walk_in",
        campaign: input.campaign ?? null,
        stage,
        stageLabel: stage,
        stageHistory: [{ stage, at: now }],
        stageEnteredAt: now,
        daysInStage: 0,
        stalled: false,
        expectedJoin: input.expectedJoin ?? null,
        expectedFee: input.expectedFee ?? "0.00",
        confidence: input.confidence ?? null,
        probability: 0,
        closeReason: null,
        closedAt: null,
        ownerRef: input.ownerRef ?? null,
        nextFollowUp: input.nextFollowUp ?? null,
        notes: input.notes ?? null,
        applicationId: null,
        version: 1,
        createdAt: now,
        updatedAt: now,
      };
      const norm = (s?: string | null) => (s ?? "").trim().toLowerCase();
      const digits = (s?: string | null) => (s ?? "").replace(/\D/g, "");
      const possibleDuplicates = rows
        .filter((r) => r.stage !== "admitted" && r.stage !== "lost")
        .map((r) => ({
          r,
          reasons: [
            norm(r.email) && norm(r.email) === norm(e.email) ? "email" : null,
            norm(r.displayName).replace(/\s+/g, " ") === norm(e.displayName).replace(/\s+/g, " ") ? "name" : null,
            digits(r.phone).length >= 7 && digits(r.phone) === digits(e.phone) ? "phone" : null,
          ].filter((x): x is string => !!x),
        }))
        .filter((x) => x.reasons.length)
        .map(({ r, reasons }) => ({ kind: "enquiry", id: r.id, number: r.enquiryNumber, name: r.displayName, reasons }));
      rows.push(e);
      return { enquiry: finish(e), possibleDuplicates };
    },
    async updateEnquiry(id, version, change) {
      const e = find(id);
      if (e.version !== version) throw new Error("stale version");
      if (change.stage && change.stage !== e.stage) {
        if (change.stage === "lost" && !change.closeReason) throw new Error("a lost enquiry needs a reason");
        move(e, change.stage, change.closeReason);
      }
      if (change.notes !== undefined) e.notes = change.notes ?? null;
      if (change.nextFollowUp !== undefined) e.nextFollowUp = change.nextFollowUp ?? null;
      e.version += 1;
      e.updatedAt = new Date().toISOString();
      return finish(e);
    },
    async linkApplication(id, version, applicationId) {
      const e = find(id);
      if (e.version !== version) throw new Error("stale version");
      e.applicationId = applicationId;
      const ids = open().map((s) => s.id);
      if (ids.includes("applied") && ids.indexOf(e.stage) < ids.indexOf("applied")) move(e, "applied");
      e.version += 1;
      return finish(e);
    },
    async funnel(_inst, window = "all") {
      const order = new Map(open().map((s, i) => [s.id, i] as const));
      const reportRows: FunnelRow[] = open().map((s) => {
        const reached = rows.filter((r) => r.stageHistory.some((h) => h.stage === s.id));
        const advanced = reached.filter((r) => r.stage === "admitted" || Math.max(...r.stageHistory.map((h) => order.get(h.stage) ?? -1)) > order.get(s.id)!).length;
        const lostHere = reached.filter((r) => r.stage === "lost" && [...r.stageHistory].reverse().find((h) => h.stage !== "lost")?.stage === s.id).length;
        return { stage: s.id, label: s.label, gone: false, entered: reached.length, advanced, lostHere, open: reached.length - advanced - lostHere, advanceRate: reached.length ? Math.round((advanced / reached.length) * 100) / 100 : null, lossShare: null };
      });
      const admitted = rows.filter((r) => r.stage === "admitted").length;
      const lost = rows.filter((r) => r.stage === "lost").length;
      return {
        stages: settings.stages,
        window,
        report: { rows: reportRows, totals: { enquiries: rows.length, admitted, lost, open: rows.length - admitted - lost, admittedRate: rows.length ? Math.round((admitted / rows.length) * 100) / 100 : null, lostShareBase: lost }, untracked: 0 },
        summary: settings.stages.map((s) => {
          const here = rows.filter((r) => r.stage === s.id);
          const value = here.reduce((n, r) => n + Number(r.expectedFee), 0);
          return { stage: s.id, label: s.label, count: here.length, value, weighted: Math.round(value * s.probability * 100) / 100 };
        }),
        generatedAt: new Date().toISOString(),
      };
    },
    async forecast(_inst, months = 6) {
      const now = new Date();
      const key = (i: number) => {
        const d = new Date(now.getFullYear(), now.getMonth() + i, 1);
        return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}`;
      };
      const admitted = rows.filter((r) => r.stage === "admitted").length;
      const lost = rows.filter((r) => r.stage === "lost").length;
      const empty = { rows: [], unrecorded: 0, total: 0 };
      return {
        months: Array.from({ length: months }, (_, i) => ({ key: key(i), count: 0, expectedJoiners: 0, best: "0.00", weighted: "0.00", admitted: "0.00" })),
        pipeline: "0.00",
        weighted: "0.00",
        winRate: { admitted, lost, rate: admitted + lost ? admitted / (admitted + lost) : null },
        bySource: [],
        byProgramme: [],
        reasonsAdmitted: empty,
        reasonsLost: empty,
        avgClosed: { admitted: null, lost: null },
        velocity: open().map((s) => ({ stage: s.id, label: s.label, days: null, count: 0 })),
        stalled: [],
        avgCycleDays: null,
        seats: settings.targets.map((t) => ({ ...t, admitted: 0, open: 0, expected: 0, remaining: t.seats, coverage: t.seats ? 0 : null })),
        staleDays: settings.staleDays,
      };
    },
    async duplicates() {
      return { pairs: [], limit: 50 };
    },
    async settings() {
      return settings;
    },
    async saveSettings(_inst, input) {
      if (input.version !== settings.version) throw new Error("stale version");
      const custom = input.stages.length ? input.stages : DEFAULT_STAGES;
      settings = { stages: [...custom, ...TERMINAL], customised: input.stages.length > 0, staleDays: input.staleDays, appliedStage: input.appliedStage ?? null, offeredStage: input.offeredStage ?? null, targets: input.targets ?? [], version: settings.version + 1 };
      return settings;
    },
  };
}
