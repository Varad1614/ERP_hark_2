/** In-memory `TimeApi` for mock mode and screen tests (the server's rules are authoritative). */
import type { TimeApi } from "./api";
import { addDaysIso, clock, human, parseDuration, weekStartOf } from "./timetrack";
import type { EffortReport, MyTimeWeek, StaffRate, TimeEntry, Timesheet, TimeProject, TimeSettings } from "./types";

const ME = { staffId: "st-me", name: "Meera Shah", number: "FAC-000101" };
const STAFF = [ME, { staffId: "st-pi", name: "Dr. Kavita Rao", number: "FAC-000007" }];
const toMin = (t: string) => Number(t.slice(0, 2)) * 60 + Number(t.slice(3, 5));
const todayIso = () => new Date().toISOString().slice(0, 10);

export function createMockTimeApi(): TimeApi {
  let seq = 0;
  let settings: TimeSettings = { weekStart: 1, roundingMinutes: 0, standardMonthlyHours: 176, defaultHourlyRate: 400, timerCapHours: 12 };
  const projects: TimeProject[] = [
    {
      id: "tp-1", institutionId: "i", code: "SERB-CRG/2026/014", name: "Monsoon rainfall nowcasting", kind: "sponsored_research",
      fundingAgency: "SERB (ANRF)", scheme: "Core Research Grant", sanctionNumber: "CRG/2026/000014", sanctionDate: "2026-01-15",
      sanctionedAmount: "3200000.00", manpowerBudget: "1200000.00", startDate: "2026-01-01", endDate: "2029-12-31",
      piStaffId: "st-pi", piName: "Dr. Kavita Rao", costCentre: "RES-SERB-14", contractId: null,
      budget: { kind: "hours", period: "total", value: "2000.00" }, budgetProgress: null, roundingMinutes: null, minimumMinutes: null,
      billableDefault: true, openToAll: false, status: "active", notes: null,
      members: [
        { staffId: "st-pi", name: "Dr. Kavita Rao", staffNumber: "FAC-000007", role: "pi", commitmentPercent: 20 },
        { staffId: "st-me", name: ME.name, staffNumber: ME.number, role: "jrf", commitmentPercent: 100 },
      ],
      loggedSeconds: 0, version: 1,
    },
    {
      id: "tp-2", institutionId: "i", code: "IQAC", name: "IQAC committee", kind: "committee", fundingAgency: null, scheme: null,
      sanctionNumber: null, sanctionDate: null, sanctionedAmount: null, manpowerBudget: null, startDate: null, endDate: null,
      piStaffId: null, piName: null, costCentre: null, contractId: null, budget: null, budgetProgress: null, roundingMinutes: null,
      minimumMinutes: null, billableDefault: false, openToAll: true, status: "active", notes: null, members: [], loggedSeconds: 0, version: 1,
    },
  ];
  const entries: TimeEntry[] = [];
  const sheets: (Timesheet & { submittedByMe: boolean })[] = [];
  let timer: { projectId: string; startedAt: number; task: string } | null = null;
  const rates = new Map<string, number>();

  const project = (id: string) => {
    const p = projects.find((x) => x.id === id);
    if (!p || p.status !== "active") throw new Error("You can log time only on active projects you belong to.");
    return p;
  };
  const weekOf = (date: string) => weekStartOf(date, settings.weekStart);
  const sheetOf = (projectId: string, week: string) => sheets.find((s) => s.staffId === ME.staffId && s.projectId === projectId && s.weekStart === week);
  const ensureOpen = (projectId: string, date: string) => {
    const s = sheetOf(projectId, weekOf(date));
    if (s && (s.status === "submitted" || s.status === "approved")) throw new Error("That week's timesheet is submitted or approved.");
  };
  const span = (e: { date: string; start: string | null; end: string | null }) => {
    if (!e.start || !e.end) return null;
    const base = Date.parse(`${e.date}T00:00:00Z`) / 60000;
    const a = base + toMin(e.start);
    let b = base + toMin(e.end);
    if (b <= a) b += 1440;
    return [a, b] as const;
  };
  const build = (input: { projectId: string; date: string; duration?: string | null; start?: string | null; end?: string | null; task?: string | null; notes?: string | null; billable?: boolean }, id?: string): TimeEntry => {
    const p = project(input.projectId);
    if ((p.startDate && input.date < p.startDate) || (p.endDate && input.date > p.endDate)) throw new Error("The date is outside the project's period.");
    const start = input.start || null;
    const end = input.end || null;
    if (!!start !== !!end) throw new Error("Give both a start and an end, or neither.");
    let seconds = input.duration ? parseDuration(input.duration) : null;
    if (input.duration && seconds == null) throw new Error("A duration such as 1:30, 1h30, 1.5 or 90m.");
    if (seconds == null && start && end) {
      const s = span({ date: input.date, start, end })!;
      seconds = (s[1] - s[0]) * 60;
    }
    if (!seconds || seconds <= 0 || seconds > 86400) throw new Error("A duration, or a start and an end.");
    const e: TimeEntry = {
      id: id ?? `te-${++seq}`, staffId: ME.staffId, staffName: ME.name, projectId: p.id, projectCode: p.code, projectName: p.name,
      date: input.date, seconds, duration: clock(seconds), start, end, endsNextDay: !!(start && end && toMin(end) <= toMin(start)),
      task: input.task ?? "", notes: input.notes ?? "", billable: input.billable ?? p.billableDefault, source: "manual", locked: false, version: 1,
    };
    const me = span(e);
    if (me) {
      const clash = entries.find((o) => o.id !== e.id && o.staffId === ME.staffId && (() => {
        const s = span(o);
        return !!s && s[0] < me[1] && me[0] < s[1];
      })());
      if (clash) throw new Error(`This time overlaps another entry of yours (${clash.projectCode}).`);
    }
    return e;
  };
  const withLocks = (e: TimeEntry): TimeEntry => {
    const s = sheetOf(e.projectId, weekOf(e.date));
    return { ...e, locked: !!s && (s.status === "submitted" || s.status === "approved") };
  };
  const view = (s: Timesheet & { submittedByMe: boolean }): Timesheet => ({
    id: s.id, staffId: s.staffId, staffName: s.staffName, projectId: s.projectId, projectCode: s.projectCode, projectName: s.projectName,
    weekStart: s.weekStart, weekEnd: s.weekEnd, status: s.status, totalSeconds: s.totalSeconds, billedSeconds: s.billedSeconds,
    submittedAt: s.submittedAt, decidedAt: s.decidedAt, decidedBy: s.decidedBy, decisionNote: s.decisionNote, version: s.version,
  });

  return {
    settings: async () => settings,
    saveSettings: async (_id, s) => (settings = s),
    projects: async () => projects.map((p) => {
      const logged = entries.filter((e) => e.projectId === p.id).reduce((n, e) => n + e.seconds, 0);
      const budgetProgress = p.budget ? { kind: p.budget.kind, period: p.budget.period, spent: logged, value: Number(p.budget.value) * 3600, ratio: logged / (Number(p.budget.value) * 3600) } : null;
      return { ...p, loggedSeconds: logged, budgetProgress };
    }),
    createProject: async (_id, input) => {
      if (projects.some((p) => p.code === input.code.toUpperCase())) throw new Error(`Project code ${input.code} is already used.`);
      const p: TimeProject = {
        id: `tp-${projects.length + 1}`, institutionId: "i", code: input.code.toUpperCase(), name: input.name, kind: input.kind,
        fundingAgency: input.fundingAgency ?? null, scheme: input.scheme ?? null, sanctionNumber: input.sanctionNumber ?? null,
        sanctionDate: input.sanctionDate ?? null, sanctionedAmount: input.sanctionedAmount ?? null, manpowerBudget: input.manpowerBudget ?? null,
        startDate: input.startDate ?? null, endDate: input.endDate ?? null, piStaffId: input.piStaffId ?? null,
        piName: STAFF.find((s) => s.staffId === input.piStaffId)?.name ?? null, costCentre: input.costCentre ?? null, contractId: null,
        budget: null, budgetProgress: null, roundingMinutes: null, minimumMinutes: null, billableDefault: input.billableDefault ?? true,
        openToAll: input.openToAll ?? false, status: "active", notes: null,
        members: input.members.map((m) => ({ staffId: m.staffId, name: STAFF.find((s) => s.staffId === m.staffId)?.name ?? m.staffId, staffNumber: "", role: m.role, commitmentPercent: m.commitmentPercent ?? null })),
        loggedSeconds: 0, version: 1,
      };
      projects.push(p);
      return p;
    },
    updateProject: async (id, _version, input) => {
      const p = projects.find((x) => x.id === id)!;
      Object.assign(p, { name: input.name, status: input.status ?? p.status, version: p.version + 1 });
      return p;
    },
    timesheets: async (_id, status) => sheets.filter((s) => !status || s.status === status).map(view),
    decide: async (id, _version, approve, reason) => {
      const s = sheets.find((x) => x.id === id)!;
      if (s.status !== "submitted") throw new Error(`A ${s.status} timesheet cannot be decided.`);
      if (!approve && !reason) throw new Error("Say why the week is returned.");
      Object.assign(s, { status: approve ? "approved" : "rejected", decidedBy: "dev|dean@sutra.dev", decisionNote: reason ?? null, version: s.version + 1 });
      return view(s);
    },
    rates: async () =>
      STAFF.map((s): StaffRate => {
        const own = rates.get(s.staffId);
        return { staffId: s.staffId, name: s.name, staffNumber: s.number, rate: own ?? null, periods: own != null ? [{ from: null, rate: own }] : [], payrollHourly: null, effectiveToday: own ?? 400, source: own != null ? "staff" : "default" };
      }),
    setRate: async (staffId, rate) => {
      if (rate == null) rates.delete(staffId);
      else rates.set(staffId, rate);
      const s = STAFF.find((x) => x.staffId === staffId)!;
      return { staffId, name: s.name, staffNumber: s.number, rate, periods: rate != null ? [{ from: null, rate }] : [], payrollHourly: null, effectiveToday: rate ?? 400, source: rate != null ? "staff" : "default" };
    },
    effort: async (_id, from, to): Promise<EffortReport> => {
      const inRange = entries.filter((e) => e.date.slice(0, 7) >= from && e.date.slice(0, 7) <= to);
      const totals = new Map<string, number>();
      for (const e of inRange) totals.set(`${e.staffId}|${e.date.slice(0, 7)}`, (totals.get(`${e.staffId}|${e.date.slice(0, 7)}`) ?? 0) + e.seconds);
      const cells = new Map<string, EffortReport["rows"][number]>();
      for (const e of inRange) {
        const k = `${e.projectId}|${e.staffId}|${e.date.slice(0, 7)}`;
        const r = cells.get(k) ?? { staffId: e.staffId, staffName: e.staffName, staffNumber: ME.number, projectId: e.projectId, projectCode: e.projectCode, projectName: e.projectName, month: e.date.slice(0, 7), seconds: 0, hours: 0, billedSeconds: 0, percent: 0, personMonths: 0, cost: "0.00" };
        r.seconds += e.seconds;
        r.billedSeconds += e.billable ? e.seconds : 0;
        cells.set(k, r);
      }
      const rows = [...cells.values()].map((r) => {
        const share = r.seconds / (totals.get(`${r.staffId}|${r.month}`) ?? r.seconds);
        const rate = rates.get(r.staffId) ?? 400;
        return { ...r, hours: Math.round((r.seconds / 36)) / 100, percent: Math.round(share * 10000) / 100, personMonths: Math.round(share * 1000) / 1000, cost: ((r.billedSeconds / 3600) * rate).toFixed(2) };
      });
      const projs = projects.filter((p) => rows.some((r) => r.projectId === p.id)).map((p) => {
        const mine = rows.filter((r) => r.projectId === p.id);
        const cost = mine.reduce((n, r) => n + Number(r.cost), 0);
        return { projectId: p.id, code: p.code, name: p.name, fundingAgency: p.fundingAgency, sanctionNumber: p.sanctionNumber, sanctionedAmount: p.sanctionedAmount, manpowerBudget: p.manpowerBudget, hours: mine.reduce((n, r) => n + r.hours, 0), personMonths: mine.reduce((n, r) => n + r.personMonths, 0), cost: cost.toFixed(2), utilisationPercent: p.manpowerBudget ? Math.round((cost / Number(p.manpowerBudget)) * 10000) / 100 : null, staffCount: new Set(mine.map((r) => r.staffId)).size };
      });
      return { from, to, approvedOnly: false, standardMonthlyHours: settings.standardMonthlyHours, rows, staffTotals: [], projects: projs };
    },
    effortCsvUrl: () => "data:text/csv;charset=utf-8,Month%2CStaff",
    entriesCsvUrl: () => "data:text/csv;charset=utf-8,Date%2CStart",
    myWeek: async (week): Promise<MyTimeWeek> => {
      const today = todayIso();
      const start = weekOf(week ?? today);
      const days = Array.from({ length: 7 }, (_, i) => addDaysIso(start, i));
      const mine = entries.filter((e) => e.date >= start && e.date <= days[6]!).map(withLocks);
      const loggable = projects.filter((p) => p.status === "active" && (p.openToAll || p.members.some((m) => m.staffId === ME.staffId)));
      const weekSeconds = mine.reduce((n, e) => n + e.seconds, 0);
      return {
        institutionId: "i", staffId: ME.staffId, today, weekStart: start, days, settings,
        projects: loggable.map((p) => {
          const daySeconds = days.map((d) => mine.filter((e) => e.projectId === p.id && e.date === d).reduce((n, e) => n + e.seconds, 0));
          const s = sheetOf(p.id, start);
          return { projectId: p.id, code: p.code, name: p.name, kind: p.kind, fundingAgency: p.fundingAgency, billableDefault: p.billableDefault, daySeconds, seconds: daySeconds.reduce((a, b) => a + b, 0), timesheet: s ? { id: s.id, status: s.status, decisionNote: s.decisionNote, version: s.version } : null };
        }),
        entries: mine,
        timer: timer ? { projectId: timer.projectId, projectCode: project(timer.projectId).code, projectName: project(timer.projectId).name, startedAt: new Date(timer.startedAt).toISOString(), elapsedSeconds: Math.floor((Date.now() - timer.startedAt) / 1000), elapsed: clock((Date.now() - timer.startedAt) / 1000), task: timer.task, notes: "", billable: true } : null,
        weekSeconds,
        weekTotal: human(weekSeconds),
      };
    },
    addEntry: async (input) => {
      ensureOpen(input.projectId, input.date);
      const e = build(input);
      entries.push(e);
      return e;
    },
    updateEntry: async (id, _version, input) => {
      const i = entries.findIndex((e) => e.id === id);
      ensureOpen(entries[i]!.projectId, entries[i]!.date);
      ensureOpen(input.projectId, input.date);
      const e = { ...build(input, id), version: entries[i]!.version + 1 };
      entries[i] = e;
      return e;
    },
    deleteEntry: async (id) => {
      const i = entries.findIndex((e) => e.id === id);
      ensureOpen(entries[i]!.projectId, entries[i]!.date);
      entries.splice(i, 1);
    },
    startTimer: async (projectId, task) => {
      if (timer) throw new Error("A timer is already running; stop it first.");
      const p = project(projectId);
      timer = { projectId, startedAt: Date.now(), task: task ?? "" };
      return { projectId, projectCode: p.code, projectName: p.name, startedAt: new Date().toISOString(), elapsedSeconds: 0, elapsed: "0:00:00", task: task ?? "", notes: "", billable: p.billableDefault };
    },
    stopTimer: async () => {
      if (!timer) throw new Error("No timer is running.");
      const seconds = Math.max(60, Math.min(Math.floor((Date.now() - timer.startedAt) / 1000), settings.timerCapHours * 3600));
      const p = project(timer.projectId);
      const e: TimeEntry = { id: `te-${++seq}`, staffId: ME.staffId, staffName: ME.name, projectId: p.id, projectCode: p.code, projectName: p.name, date: todayIso(), seconds, duration: clock(seconds), start: null, end: null, endsNextDay: false, task: timer.task, notes: "", billable: p.billableDefault, source: "timer", locked: false, version: 1 };
      entries.push(e);
      timer = null;
      return e;
    },
    discardTimer: async () => {
      if (!timer) throw new Error("No timer is running.");
      timer = null;
    },
    submitWeek: async (projectId, week) => {
      const start = weekOf(week);
      const total = entries.filter((e) => e.projectId === projectId && weekOf(e.date) === start).reduce((n, e) => n + e.seconds, 0);
      if (!total) throw new Error("There is no time on this project in that week.");
      const p = project(projectId);
      let s = sheetOf(projectId, start);
      if (s && (s.status === "submitted" || s.status === "approved")) throw new Error(`This timesheet is already ${s.status}.`);
      if (!s) {
        s = { id: `ts-${++seq}`, staffId: ME.staffId, staffName: ME.name, projectId, projectCode: p.code, projectName: p.name, weekStart: start, weekEnd: addDaysIso(start, 6), status: "submitted", totalSeconds: total, billedSeconds: total, submittedAt: new Date().toISOString(), decidedAt: null, decidedBy: null, decisionNote: null, version: 1, submittedByMe: true };
        sheets.push(s);
      } else {
        Object.assign(s, { status: "submitted", totalSeconds: total, version: s.version + 1 });
      }
      return view(s);
    },
    withdraw: async (id) => {
      const s = sheets.find((x) => x.id === id)!;
      Object.assign(s, { status: "draft", version: s.version + 1 });
      return view(s);
    },
    myApprovals: async () => sheets.filter((s) => s.status === "submitted" && !s.submittedByMe).map(view),
    myCsvUrl: () => "data:text/csv;charset=utf-8,Date%2CStart",
    importCsv: async (csv, dryRun) => {
      const lines = csv.split(/\r?\n/).filter((l) => l.trim());
      const rows = Math.max(0, lines.length - 1);
      return { source: /start date/i.test(lines[0] ?? "") ? "Toggl" : "CSV", mapping: {}, headers: (lines[0] ?? "").split(","), rows, added: rows, skipped: 0, invalid: [], dryRun, seconds: 0 };
    },
  };
}
