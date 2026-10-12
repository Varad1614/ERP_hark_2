/**
 * In-memory `InstitutionApi` for `mock` mode and the screen tests. It mirrors the server rules
 * the screens depend on (lifecycle transitions → 409, `If-Match` → 412, readiness keys of all
 * three providers, calendar publication / open / assignment / simulation), not every detail.
 */
import { ApiProblem } from "@sutra/api-client";
import type { InstitutionApi } from "./api";
import type {
  AcademicStructure,
  AcademicYear,
  Calendar,
  CalendarAssignment,
  CalendarException,
  CalendarSimulation,
  CalendarVersion,
  Campus,
  ClosureEvidence,
  Curriculum,
  CurriculumVersion,
  CurriculumVersionContent,
  ImpactItem,
  Institution,
  InstitutionStatus,
  OrgTypeBlueprint,
  OrgUnit,
  Policy,
  Position,
  Programme,
  ReadinessItem,
  SchoolModel,
} from "./types";

let seq = 0;
const uid = () => `01920000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const now = () => new Date().toISOString();
const today = () => new Date().toISOString().slice(0, 10);

function problem(status: number, detail: string, errorCode = status === 409 ? "conflict" : status === 412 ? "precondition-failed" : status === 404 ? "not-found" : "validation-failed"): never {
  throw new ApiProblem({ type: "about:blank", title: detail, status, detail, errorCode });
}

function check<T extends { version: number }>(row: T | undefined, version: number, what = "record"): T {
  if (!row) problem(404, `${what} not found`);
  if (row.version !== version) problem(412, `${what} was changed by someone else — reload and try again`);
  return row;
}

function bump<T extends { version: number }>(row: T, patch: Partial<T>): T {
  Object.assign(row, patch, { version: row.version + 1 });
  return { ...row };
}

const INSTITUTION_MOVES: Record<string, Partial<Record<InstitutionStatus, InstitutionStatus>>> = {
  configure: { draft: "configured", returned: "configured", rejected: "configured" },
  submit: { configured: "pending_approval" },
  suspend: { active: "suspended" },
  reactivate: { suspended: "active" },
  archive: { active: "archived", suspended: "archived" },
};

export function createMockInstitutionApi(): InstitutionApi {
  const institutions = new Map<string, Institution>();
  const blueprints = new Map<string, OrgTypeBlueprint>();
  const orgUnits = new Map<string, OrgUnit>();
  const positions = new Map<string, Position>();
  const policies = new Map<string, Policy>();
  const campuses = new Map<string, Campus>();
  const structures = new Map<string, AcademicStructure>();
  const programmes = new Map<string, Programme>();
  const curricula = new Map<string, Curriculum>();
  const curriculumVersions = new Map<string, CurriculumVersion>();
  const schoolModels = new Map<string, SchoolModel>();
  const calendars = new Map<string, Calendar>();
  const years = new Map<string, AcademicYear>();
  const versions = new Map<string, CalendarVersion>();
  const assignments = new Map<string, CalendarAssignment>();
  const exceptions = new Map<string, CalendarException>();
  const simulations = new Map<string, CalendarSimulation>();

  const of = <T extends { institutionId: string }>(m: Map<string, T>, inst: string) => [...m.values()].filter((x) => x.institutionId === inst).map((x) => ({ ...x }));
  const editable = (inst: string) => {
    const i = institutions.get(inst);
    if (!i) problem(404, "institution not found");
    if (i.status === "suspended" || i.status === "archived") problem(409, `institution is ${i.status}; its structure is read-only`);
    return i;
  };
  const move = <T extends { status: string; version: number }>(row: T | undefined, version: number, moves: Record<string, string>, what: string): T => {
    const r = check(row, version, what);
    const to = moves[r.status];
    if (!to) problem(409, `${what} cannot change from ${r.status}`);
    return bump(r, { status: to } as Partial<T>);
  };

  function missingProfile(i: Institution): string[] {
    const p = i.profile;
    const blank = (v: string | null | undefined) => !v?.trim();
    return [
      blank(p.shortName) && "shortName",
      !p.institutionType && "institutionType",
      blank(p.registrationNumber) && "registrationNumber",
      blank(p.address) && "address",
      blank(p.contactPhone) && "contactPhone",
      blank(p.officialEmail) && "officialEmail",
      !p.academicPattern && "academicPattern",
      p.workingDays.length === 0 && "workingDays",
      blank(p.administratorEmail) && "administratorEmail",
    ].filter((x): x is string => !!x);
  }

  function newVersion(c: Curriculum, content: CurriculumVersionContent, supersedes: string | null): CurriculumVersion {
    const n = [...curriculumVersions.values()].filter((x) => x.curriculumId === c.id).length;
    const components = content.components ?? [];
    const v: CurriculumVersion = {
      ...content,
      components,
      id: uid(),
      institutionId: c.institutionId,
      curriculumId: c.id,
      programmeId: c.programmeId,
      status: "draft",
      versionNo: n + 1,
      supersedesVersionId: supersedes,
      totalCredits: components.reduce((t, x) => t + x.credits, 0),
      version: 1,
      createdAt: now(),
      updatedAt: now(),
    };
    curriculumVersions.set(v.id, v);
    return v;
  }

  function readinessItems(inst: string): ReadinessItem[] {
    const i = institutions.get(inst);
    const item = (key: string, area: ReadinessItem["area"], label: string, required: boolean, met: boolean | null, link: string): ReadinessItem => ({
      key,
      area,
      label,
      required,
      status: met === null ? "not_applicable" : met ? "met" : "unmet",
      detail: null,
      link,
    });
    const units = of(orgUnits, inst);
    const vs = of(versions, inst);
    return [
      item("institution.profile_complete", "institution", "Institution profile is complete", true, !!i && missingProfile(i).length === 0, "/campus/institution"),
      item("institution.timezone_set", "institution", "Business timezone is set", true, !!i?.timezone, "/campus/institution"),
      item("institution.policies_effective", "institution", "Institutional policies are effective", false, of(policies, inst).some((p) => p.status === "effective"), "/campus/policies"),
      item("organisation.root_unit_active", "organisation", "Root organisation unit is active", true, units.some((u) => !u.parentId && u.status === "active"), "/campus/organisation"),
      item("organisation.governance_active", "organisation", "Governance is configured", false, false, "/campus/organisation"),
      item("position.definitions_active", "organisation", "Positions are defined", false, of(positions, inst).some((p) => p.status === "active"), "/campus/positions"),
      item("campus.active_campus", "campus", "At least one active campus", true, of(campuses, inst).some((c) => c.status === "active"), "/campus/campuses"),
      item("curriculum.academic_structure_published", "curriculum", "Academic structure is published", true, of(structures, inst).some((s) => s.status === "published"), "/academics/structure"),
      item("curriculum.programme_published", "curriculum", "At least one programme is published", true, of(programmes, inst).some((p) => p.status === "published"), "/academics/programmes"),
      item("curriculum.curriculum_published", "curriculum", "At least one curriculum version is published", true, of(curriculumVersions, inst).some((v) => v.status === "published"), "/academics/curriculum"),
      item("curriculum.school_model_published", "curriculum", "School academic model is published", true, null, "/academics/school-model"),
      item("calendar.version_published", "calendar", "An academic calendar version is published", true, vs.some((v) => v.status === "published"), "/academics/calendar"),
      item(
        "calendar.academic_year_open",
        "calendar",
        "An academic year is open (or planned with a published calendar)",
        true,
        of(years, inst).some((y) => y.status === "open" || (y.status === "planned" && vs.some((v) => v.academicYearId === y.id && v.status === "published"))),
        "/academics/calendar",
      ),
      item(
        "calendar.assignment_effective",
        "calendar",
        "The calendar is assigned to the institution",
        true,
        of(assignments, inst).some((a) => a.scopeKind === "institution" && a.status === "effective"),
        "/academics/calendar",
      ),
    ];
  }

  function version(id: string): CalendarVersion {
    const v = versions.get(id);
    if (!v) problem(404, "calendar version not found");
    return v;
  }
  function draft(id: string, ver: number): CalendarVersion {
    const v = check(versions.get(id), ver, "calendar version");
    if (v.status !== "draft") problem(409, `calendar version is ${v.status}; only a draft can be edited`);
    return v;
  }

  const api: InstitutionApi = {
    async listInstitutions() {
      return [...institutions.values()].map((i) => ({ ...i }));
    },
    async getInstitution(id) {
      const i = institutions.get(id);
      if (!i) problem(404, "institution not found");
      return { ...i };
    },
    async registerInstitution(input) {
      const errors = [];
      if (!/^[A-Z0-9-]{2,32}$/.test(input.code.trim())) errors.push({ field: "code", code: "format", message: "code must be 2-32 characters of A-Z, 0-9 or '-'" });
      if (!input.name.trim()) errors.push({ field: "name", code: "required", message: "name is required" });
      if (errors.length) throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: errors });
      if ([...institutions.values()].some((i) => i.code === input.code.trim())) problem(409, "an institution with this code already exists");
      const inst: Institution = {
        id: uid(),
        tenantId: "00000000-0000-7000-8000-00000000d0e0",
        code: input.code.trim(),
        name: input.name.trim(),
        timezone: input.timezone || "Asia/Kolkata",
        status: "draft",
        statusChangedAt: now(),
        profile: { regulatoryBodies: [], workingDays: [], revision: 0 },
        version: 1,
        createdAt: now(),
        updatedAt: now(),
      };
      institutions.set(inst.id, inst);
      return { ...inst };
    },
    async renameInstitution(id, ver, name) {
      return bump(check(institutions.get(id), ver, "institution"), { name, updatedAt: now() });
    },
    async reviseProfile(id, ver, input) {
      const i = check(institutions.get(id), ver, "institution");
      const defined = Object.fromEntries(Object.entries(input).filter(([, v]) => v !== undefined));
      return bump(i, { profile: { ...i.profile, ...defined, revision: i.profile.revision + 1 } as Institution["profile"], updatedAt: now() });
    },
    async institutionCommand(id, ver, cmd) {
      const i = check(institutions.get(id), ver, "institution");
      const to = INSTITUTION_MOVES[cmd]?.[i.status];
      if (!to) problem(409, `institution cannot ${cmd} while ${i.status}`);
      if (cmd === "configure" && missingProfile(i).length) problem(422, `profile incomplete: ${missingProfile(i).join(", ")}`);
      if (cmd === "submit") {
        const blocking = readinessItems(id).filter((x) => x.required && x.status === "unmet");
        if (blocking.length) problem(409, `activation readiness incomplete: ${blocking.map((b) => b.key).join(", ")}`);
        // No activation authority configured (NoopWorkflowAdapter): auto-approved, as the server.
        return bump(i, { status: "active", statusChangedAt: now(), updatedAt: now() });
      }
      return bump(i, { status: to, statusChangedAt: now(), updatedAt: now() });
    },
    async decideActivation(id, ver, decision) {
      const i = check(institutions.get(id), ver, "institution");
      if (i.status !== "pending_approval") problem(409, "no activation request awaits a decision");
      const to: InstitutionStatus = decision === "approve" ? "active" : decision === "reject" ? "rejected" : "returned";
      return bump(i, { status: to, updatedAt: now() });
    },

    async listBlueprints(inst) {
      return of(blueprints, inst);
    },
    async createBlueprint(inst, input) {
      editable(inst);
      const b: OrgTypeBlueprint = { id: uid(), institutionId: inst, versionNo: of(blueprints, inst).length + 1, status: "draft", effectiveFrom: input.effectiveFrom, unitTypes: input.unitTypes, version: 1, createdAt: now(), updatedAt: now() };
      blueprints.set(b.id, b);
      return { ...b };
    },
    async publishBlueprint(id, ver) {
      const b = check(blueprints.get(id), ver, "blueprint");
      for (const o of blueprints.values()) if (o.institutionId === b.institutionId && o.status === "published") bump(o, { status: "superseded" });
      return move(blueprints.get(id), ver, { draft: "published" }, "blueprint");
    },
    async listOrgUnits(inst) {
      return of(orgUnits, inst);
    },
    async createOrgUnit(inst, input) {
      editable(inst);
      const bp = of(blueprints, inst).find((b) => b.status === "published");
      if (!bp) problem(409, "no published organisation type blueprint is effective; publish the unit types first");
      if (!bp.unitTypes.some((t) => t.key === input.unitType)) problem(422, `unit type ${input.unitType} is not in the blueprint`);
      const parent = input.parentId ? orgUnits.get(input.parentId) : undefined;
      const u: OrgUnit = { id: uid(), institutionId: inst, blueprintId: bp.id, parentId: input.parentId ?? null, unitType: input.unitType, code: input.code, name: input.name, depth: parent ? parent.depth + 1 : 0, path: parent ? `${parent.path}/${input.code}` : input.code, status: "draft", version: 1, createdAt: now(), updatedAt: now() };
      orgUnits.set(u.id, u);
      return { ...u };
    },
    async orgUnitCommand(id, ver, cmd) {
      const moves: Record<string, Record<string, string>> = {
        activate: { draft: "active" },
        suspend: { active: "suspended" },
        restore: { suspended: "active" },
        decommission: { active: "decommissioned", suspended: "decommissioned" },
        archive: { decommissioned: "archived" },
      };
      return move(orgUnits.get(id), ver, moves[cmd] ?? {}, "organisation unit");
    },
    async listPositions(inst) {
      return of(positions, inst);
    },
    async createPosition(inst, input) {
      editable(inst);
      const p: Position = { id: uid(), institutionId: inst, orgUnitId: input.orgUnitId, code: input.code, title: input.title, status: "draft", revision: 1, latestRevision: 1, headcount: input.headcount ?? 1, asOf: today(), effectiveFrom: input.effectiveFrom, effectiveTo: null, version: 1, createdAt: now(), updatedAt: now() };
      positions.set(p.id, p);
      return { ...p };
    },
    async positionCommand(id, ver, cmd) {
      const moves: Record<string, Record<string, string>> = { activate: { draft: "active" }, retire: { active: "retired" }, archive: { retired: "archived" } };
      return move(positions.get(id), ver, moves[cmd] ?? {}, "position");
    },
    async listPolicies(inst) {
      return of(policies, inst);
    },
    async createPolicy(inst, input) {
      editable(inst);
      const p: Policy = { id: uid(), institutionId: inst, key: input.key, title: input.title, status: "draft", revision: 1, effectiveFrom: input.effectiveFrom, version: 1, createdAt: now(), updatedAt: now() };
      policies.set(p.id, p);
      return { ...p };
    },
    async publishPolicy(id, ver) {
      return move(policies.get(id), ver, { draft: "effective" }, "policy");
    },

    async listCampuses(inst) {
      return of(campuses, inst);
    },
    async registerCampus(inst, input) {
      editable(inst);
      const c: Campus = { id: uid(), institutionId: inst, code: input.code, name: input.name, campusType: input.campusType, isMain: input.isMain ?? false, address: input.address ?? {}, timezone: input.timezone || null, status: "registered", statusChangedAt: now(), version: 1, createdAt: now(), updatedAt: now() };
      campuses.set(c.id, c);
      return { ...c };
    },
    async campusCommand(id, ver, cmd, body) {
      if (cmd === "suspend" && !body?.reason?.trim()) problem(422, "a reason is required");
      if (cmd === "decommission" && (!body?.reason?.trim() || !body.effectiveOn)) problem(422, "a reason and an effective date are required");
      const moves: Record<string, Record<string, string>> = {
        activate: { registered: "active", pending_activation: "active" },
        suspend: { active: "suspended" },
        restore: { suspended: "active" },
        decommission: { active: "decommissioned", suspended: "decommissioned" },
        archive: { decommissioned: "archived" },
      };
      return move(campuses.get(id), ver, moves[cmd] ?? {}, "campus");
    },
    async listStructures(inst) {
      return of(structures, inst);
    },
    async createStructure(inst, input) {
      editable(inst);
      const s: AcademicStructure = { id: uid(), institutionId: inst, name: input.name, model: input.model, nepAligned: input.nepAligned ?? false, nodes: input.nodes ?? [], effectiveFrom: input.effectiveFrom ?? null, status: "draft", versionNo: of(structures, inst).length + 1, version: 1, createdAt: now(), updatedAt: now() };
      structures.set(s.id, s);
      return { ...s };
    },
    async structureCommand(id, ver, cmd) {
      return move(structures.get(id), ver, cmd === "publish" ? { draft: "published" } : { published: "retired" }, "academic structure");
    },
    async listProgrammes(inst) {
      return of(programmes, inst);
    },
    async createProgramme(inst, input) {
      editable(inst);
      const p: Programme = { id: uid(), institutionId: inst, code: input.code, name: input.name, level: input.level, degreeType: input.degreeType ?? null, durationTerms: input.durationTerms, orgUnitId: input.orgUnitId ?? null, campusIds: input.campusIds ?? [], status: "draft", versionNo: 1, version: 1, createdAt: now(), updatedAt: now() };
      programmes.set(p.id, p);
      return { ...p };
    },
    async programmeCommand(id, ver, cmd) {
      const p = programmes.get(id);
      if (cmd === "publish" && p && !of(structures, p.institutionId).some((s) => s.status === "published" && s.nodes.some((n) => n.kind === "level" && n.key === p.level)))
        problem(409, `level ${p.level} is not in the published academic structure`);
      return move(p, ver, cmd === "publish" ? { draft: "published" } : { published: "retired" }, "programme");
    },
    async listCurricula(programmeId) {
      return [...curricula.values()].filter((c) => c.programmeId === programmeId).map((c) => ({ ...c }));
    },
    async createCurriculum(programmeId, input) {
      const p = programmes.get(programmeId);
      if (!p) problem(404, "programme not found");
      const c: Curriculum = { id: uid(), institutionId: p.institutionId, programmeId, code: input.code, title: input.title, status: "draft", version: 1, createdAt: now(), updatedAt: now() };
      const v = newVersion(c, input.initialVersion, null);
      c.openVersionId = v.id;
      curricula.set(c.id, c);
      return { ...c };
    },
    async listCurriculumVersions(curriculumId) {
      return [...curriculumVersions.values()].filter((v) => v.curriculumId === curriculumId).map((v) => ({ ...v }));
    },
    async reviseCurriculum(curriculumId, input) {
      const c = curricula.get(curriculumId);
      if (!c) problem(404, "curriculum not found");
      const cur = [...curriculumVersions.values()].find((v) => v.curriculumId === curriculumId && v.status === "published");
      if (!cur) problem(409, "only a published curriculum can be revised");
      return { ...newVersion(c, { ...cur, effectiveFrom: input.effectiveFrom, changeSummary: input.changeSummary, appliesFromCohort: input.appliesFromCohort ?? cur.appliesFromCohort }, cur.id) };
    },
    async curriculumVersionCommand(id, ver, cmd) {
      const v = check(curriculumVersions.get(id), ver, "curriculum version");
      if (cmd === "publish") {
        if (v.status !== "draft") problem(409, `curriculum version is ${v.status}`);
        const p = programmes.get(v.programmeId);
        if (p?.status !== "published") problem(409, "publish the programme before its curriculum");
        if ((v.components ?? []).length === 0) problem(409, "the curriculum version has no course slots");
        for (const o of curriculumVersions.values()) if (o.curriculumId === v.curriculumId && o.status === "published") bump(o, { status: "superseded" });
        const c = curricula.get(v.curriculumId);
        if (c) Object.assign(c, { status: "published", currentVersionId: v.id, openVersionId: null });
        return bump(v, { status: "published" });
      }
      if (v.status !== "published") problem(409, `curriculum version is ${v.status}`);
      return bump(v, { status: "retired" });
    },
    async listSchoolModels(inst) {
      return of(schoolModels, inst);
    },
    async createSchoolModel(inst, input) {
      editable(inst);
      const m: SchoolModel = { id: uid(), institutionId: inst, name: input.name, nepStageAligned: input.nepStageAligned ?? false, status: "draft", versionNo: of(schoolModels, inst).length + 1, board: input.board ?? null, gradeLevels: input.gradeLevels ?? [], version: 1, createdAt: now(), updatedAt: now() };
      schoolModels.set(m.id, m);
      return { ...m };
    },
    async schoolModelCommand(id, ver) {
      return move(schoolModels.get(id), ver, { draft: "published" }, "school model");
    },

    async readiness(inst) {
      const i = institutions.get(inst);
      if (!i) problem(404, "institution not found");
      const items = readinessItems(inst);
      return { institutionId: inst, institutionStatus: i.status, ready: !items.some((x) => x.required && x.status === "unmet"), items, evaluatedAt: now() };
    },
    async listCalendars(inst) {
      return of(calendars, inst);
    },
    async createCalendar(inst, input) {
      editable(inst);
      if (!/^[A-Z0-9-]{1,32}$/.test(input.code.trim()))
        throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "code", code: "format", message: "code must be 1-32 characters of A-Z, 0-9 or '-'" }] });
      const c: Calendar = { id: uid(), institutionId: inst, code: input.code.trim(), name: input.name.trim(), description: input.description ?? null, status: "draft", statusReason: null, retiredAt: null, version: 1, createdAt: now(), updatedAt: now() };
      calendars.set(c.id, c);
      return { ...c };
    },
    async retireCalendar(id, ver) {
      const c = check(calendars.get(id), ver, "calendar");
      if ([...assignments.values()].some((a) => a.calendarId === id && a.status === "effective")) problem(409, "retire or end the effective assignments of this calendar first");
      if ([...years.values()].some((y) => y.calendarId === id && (y.status === "open" || y.status === "closing"))) problem(409, "close the open academic year of this calendar first");
      for (const v of versions.values()) if (v.calendarId === id && (v.status === "published" || v.status === "draft")) bump(v, { status: "retired" });
      return bump(c, { status: "retired", retiredAt: now() });
    },
    async listYears(inst) {
      return of(years, inst).sort((a, b) => a.startsOn.localeCompare(b.startsOn));
    },
    async createYear(inst, input) {
      editable(inst);
      if (input.endsOn <= input.startsOn)
        throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "endsOn", code: "range", message: "the end date must be after the start date" }] });
      const clash = [...years.values()].find((y) => y.calendarId === input.calendarId && y.startsOn <= input.endsOn && y.endsOn >= input.startsOn);
      if (clash) problem(409, `the dates overlap academic year ${clash.label} of this calendar`);
      const y: AcademicYear = { id: uid(), institutionId: inst, calendarId: input.calendarId, label: input.label, status: "planned", startsOn: input.startsOn, endsOn: input.endsOn, predecessorYearId: null, successorYearId: null, openedAt: null, closedAt: null, version: 1, createdAt: now(), updatedAt: now() };
      years.set(y.id, y);
      return { ...y };
    },
    async openYear(id, ver) {
      const y = check(years.get(id), ver, "academic year");
      if (y.status !== "planned") problem(409, `academic year cannot move from ${y.status} to open`);
      if (![...versions.values()].some((v) => v.academicYearId === id && v.status === "published")) problem(409, "publish a calendar version for this academic year before opening it");
      return bump(y, { status: "open", openedAt: now() });
    },
    async closeYear(id, ver, input) {
      const y = check(years.get(id), ver, "academic year");
      if (y.status !== "open") problem(409, `academic year cannot move from ${y.status} to closing`);
      const ended = today() > y.endsOn;
      const early = input.earlyClosureReason?.trim();
      if (!ended && !early) problem(409, `the academic year cannot close yet: calendar.year_ended (the year runs until ${y.endsOn})`);
      const evidence: ClosureEvidence[] = [
        { key: "calendar.year_ended", owner: "academic_calendar", mandatory: true, status: ended ? "satisfied" : "waived", detail: ended ? `the year ended on ${y.endsOn}` : `early closure permitted: ${early}` },
        { key: "assessment.results_finalised", owner: "assessment", mandatory: false, status: "not_available", detail: "the assessment domain is not deployed yet" },
      ];
      let successorYearId: string | null = null;
      if (input.prepareSuccessor !== false) {
        const end = new Date(`${y.endsOn}T00:00:00Z`);
        const start = new Date(end.getTime() + 86_400_000).toISOString().slice(0, 10);
        end.setUTCFullYear(end.getUTCFullYear() + 1);
        const label = /^\d{4}-\d{2}$/.test(y.label) ? `${Number(y.label.slice(0, 4)) + 1}-${String((Number(y.label.slice(5)) + 1) % 100).padStart(2, "0")}` : `${y.label} (next)`;
        const s: AcademicYear = { ...y, id: uid(), label, status: "planned", startsOn: start, endsOn: end.toISOString().slice(0, 10), predecessorYearId: y.id, successorYearId: null, openedAt: null, closedAt: null, version: 1 };
        years.set(s.id, s);
        successorYearId = s.id;
      }
      return { year: bump(y, { status: "closed", closedAt: now(), successorYearId }), evidence };
    },
    async listVersions(calendarId) {
      return [...versions.values()].filter((v) => v.calendarId === calendarId).sort((a, b) => a.versionNo - b.versionNo).map((v) => ({ ...v, terms: [...v.terms], keyDates: [...v.keyDates] }));
    },
    async createVersion(calendarId, input) {
      const c = calendars.get(calendarId);
      if (!c) problem(404, "calendar not found");
      if (c.status === "retired") problem(409, "the calendar is retired; it accepts no new use");
      const y = years.get(input.academicYearId);
      if (!y) problem(422, "unknown academic year");
      if ([...versions.values()].some((v) => v.academicYearId === y.id && v.status === "draft")) problem(409, "this academic year already has a draft calendar version");
      const n = [...versions.values()].filter((v) => v.calendarId === calendarId).length;
      const i = institutions.get(c.institutionId);
      const v: CalendarVersion = {
        id: uid(),
        calendarId,
        institutionId: c.institutionId,
        academicYearId: y.id,
        versionNo: n + 1,
        status: "draft",
        supersedesVersionId: null,
        startsOn: input.startsOn ?? y.startsOn,
        endsOn: input.endsOn ?? y.endsOn,
        timezone: i?.timezone ?? "Asia/Kolkata",
        notes: input.notes ?? null,
        processInstanceId: null,
        statusReason: null,
        publishedAt: null,
        retiredAt: null,
        terms: [],
        keyDates: [],
        version: 1,
        createdAt: now(),
        updatedAt: now(),
      };
      versions.set(v.id, v);
      return { ...v };
    },
    async addTerm(id, ver, input) {
      const v = draft(id, ver);
      if (input.endsOn <= input.startsOn)
        throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: [{ field: "endsOn", code: "range", message: "the end date must be after the start date" }] });
      if (v.terms.some((t) => t.code === input.code)) problem(409, `term ${input.code} already exists in this version`);
      return bump(v, { terms: [...v.terms, { id: uid(), ...input }].sort((a, b) => a.sequence - b.sequence) });
    },
    async removeTerm(id, ver, termId) {
      const v = draft(id, ver);
      return bump(v, { terms: v.terms.filter((t) => t.id !== termId) });
    },
    async addKeyDate(id, ver, input) {
      const v = draft(id, ver);
      return bump(v, { keyDates: [...v.keyDates, { id: uid(), kind: input.kind, name: input.name, startsOn: input.startsOn, endsOn: input.endsOn ?? input.startsOn }] });
    },
    async removeKeyDate(id, ver, keyDateId) {
      const v = draft(id, ver);
      return bump(v, { keyDates: v.keyDates.filter((k) => k.id !== keyDateId) });
    },
    async versionCommand(id, ver, cmd) {
      const v = check(versions.get(id), ver, "calendar version");
      if (cmd === "publish") {
        if (v.status !== "draft") problem(409, `calendar version cannot move from ${v.status} to in_review`);
        const errors = [];
        if (v.terms.length === 0) errors.push({ field: "terms", code: "required", message: "add at least one term" });
        for (const t of v.terms) if (t.startsOn < v.startsOn || t.endsOn > v.endsOn) errors.push({ field: "terms", code: "outside-version", message: `term ${t.code} lies outside the version period` });
        if (errors.length) throw new ApiProblem({ type: "about:blank", title: "Validation failed", status: 422, errorCode: "validation-failed", validationErrors: errors });
        for (const o of versions.values()) if (o.calendarId === v.calendarId && o.academicYearId === v.academicYearId && o.status === "published") bump(o, { status: "superseded" });
        const c = calendars.get(v.calendarId);
        if (c && c.status === "draft") bump(c, { status: "published" });
        return bump(v, { status: "published", publishedAt: now(), processInstanceId: `noop-${uid()}` });
      }
      if (cmd === "revise") {
        if (v.status !== "published") problem(409, `only the published version can be revised (this one is ${v.status})`);
        if ([...versions.values()].some((o) => o.academicYearId === v.academicYearId && o.status === "draft")) problem(409, "this academic year already has a draft calendar version");
        const n = [...versions.values()].filter((o) => o.calendarId === v.calendarId).length;
        const s: CalendarVersion = { ...v, id: uid(), versionNo: n + 1, status: "draft", supersedesVersionId: v.id, publishedAt: null, processInstanceId: null, terms: v.terms.map((t) => ({ ...t, id: uid() })), keyDates: v.keyDates.map((k) => ({ ...k, id: uid() })), version: 1 };
        versions.set(s.id, s);
        return { ...s };
      }
      if (v.status !== "published" && v.status !== "draft") problem(409, `calendar version cannot move from ${v.status} to retired`);
      if ([...assignments.values()].some((a) => a.calendarVersionId === id && a.status === "effective")) problem(409, "effective assignments still govern through this version");
      return bump(v, { status: "retired", retiredAt: now() });
    },
    async listAssignments(calendarId) {
      return [...assignments.values()].filter((a) => a.calendarId === calendarId).map((a) => ({ ...a }));
    },
    async createAssignment(calendarId, input) {
      const c = calendars.get(calendarId);
      if (!c) problem(404, "calendar not found");
      const a: CalendarAssignment = {
        id: uid(),
        institutionId: c.institutionId,
        calendarId,
        calendarVersionId: input.calendarVersionId,
        scopeKind: input.scopeKind,
        scopeId: input.scopeId ?? c.institutionId,
        effectiveFrom: input.effectiveFrom,
        effectiveTo: input.effectiveTo ?? null,
        status: "draft",
        statusReason: null,
        governing: false,
        activatedAt: null,
        retiredAt: null,
        version: 1,
        createdAt: now(),
        updatedAt: now(),
      };
      assignments.set(a.id, a);
      return { ...a };
    },
    async assignmentCommand(id, ver, cmd) {
      const a = check(assignments.get(id), ver, "calendar assignment");
      if (cmd === "activate") {
        if (a.status !== "draft") problem(409, `calendar assignment cannot move from ${a.status} to effective`);
        if (version(a.calendarVersionId).status !== "published") problem(409, "only a published version can govern a scope");
        const clash = [...assignments.values()].find((o) => o.id !== id && o.status === "effective" && o.scopeKind === a.scopeKind && o.scopeId === a.scopeId);
        if (clash) problem(409, "another effective assignment already governs this scope in an overlapping period");
        return bump(a, { status: "effective", activatedAt: now(), governing: a.effectiveFrom <= today() });
      }
      if (a.status === "retired") problem(409, "calendar assignment is already retired");
      return bump(a, { status: "retired", retiredAt: now(), governing: false });
    },
    async listExceptions(calendarId) {
      return [...exceptions.values()].filter((e) => e.calendarId === calendarId).map((e) => ({ ...e }));
    },
    async createException(calendarId, input) {
      const c = calendars.get(calendarId);
      if (!c) problem(404, "calendar not found");
      if (version(input.calendarVersionId).status !== "published") problem(409, "an exception needs a published base version");
      const e: CalendarException = {
        id: uid(),
        institutionId: c.institutionId,
        calendarId,
        calendarVersionId: input.calendarVersionId,
        kind: input.kind,
        scopeKind: input.scopeKind ?? "institution",
        scopeId: input.scopeId ?? c.institutionId,
        startsOn: input.startsOn,
        endsOn: input.endsOn,
        reason: input.reason,
        ownerSubject: "dev|admin@sutra.dev",
        status: "approved",
        governing: false,
        processInstanceId: `noop-${uid()}`,
        decisionReason: null,
        decidedAt: now(),
        appliedAt: null,
        completedAt: null,
        version: 2,
        createdAt: now(),
        updatedAt: now(),
      };
      exceptions.set(e.id, e);
      return { ...e };
    },
    async exceptionCommand(id, ver, cmd) {
      const e = check(exceptions.get(id), ver, "calendar exception");
      if (cmd === "apply") {
        if (e.status !== "approved") problem(409, `calendar exception cannot move from ${e.status} to in_effect`);
        if (today() < e.startsOn) problem(409, `the exception starts on ${e.startsOn}`);
        return bump(e, { status: "in_effect", appliedAt: now(), governing: true });
      }
      if (cmd === "complete") {
        if (e.status !== "approved" && e.status !== "in_effect") problem(409, `calendar exception cannot move from ${e.status} to completed`);
        return bump(e, { status: "completed", completedAt: now(), governing: false });
      }
      if (e.status !== "approved" && e.status !== "requested") problem(409, `calendar exception cannot move from ${e.status} to withdrawn`);
      return bump(e, { status: "withdrawn" });
    },
    async simulate(calendarId, baselineVersionId, proposal) {
      const v = version(baselineVersionId);
      const y = years.get(v.academicYearId);
      const starts = proposal.startsOn ?? v.startsOn;
      const ends = proposal.endsOn ?? v.endsOn;
      const items: ImpactItem[] = [];
      if (starts !== v.startsOn || ends !== v.endsOn) items.push({ code: "period.changed", severity: "info", message: `period ${v.startsOn} – ${v.endsOn} becomes ${starts} – ${ends}`, affected: [], provenance: "proposal" });
      if (y && (starts < y.startsOn || ends > y.endsOn)) items.push({ code: "period.outside_year", severity: "blocking", message: `the proposed period leaves the academic year (${y.startsOn} – ${y.endsOn})`, affected: [], provenance: "proposal" });
      for (const t of v.terms) if (t.startsOn < starts || t.endsOn > ends) items.push({ code: "terms.outside_version", severity: "blocking", message: `term ${t.code} lies outside the version period`, affected: [], provenance: "proposal" });
      const governing = [...assignments.values()].filter((a) => a.calendarVersionId === v.id && a.status === "effective");
      if (items.length && governing.length) items.push({ code: "downstream.unknown", severity: "uncertain", message: "the calendar governs scopes whose timetables, attendance and exams are not yet visible to the simulation", affected: governing.map((a) => a.id), provenance: "timetable, attendance, assessment (not deployed)" });
      const outcome = items.some((i) => i.severity === "blocking") ? "unfavourable" : items.some((i) => i.severity === "uncertain") ? "uncertain" : "favourable";
      const s: CalendarSimulation = {
        id: uid(),
        institutionId: v.institutionId,
        calendarId,
        baselineVersionId,
        status: "evaluated",
        outcome,
        baseline: {
          calendarVersionId: v.id,
          versionNo: v.versionNo,
          status: v.status,
          aggregateVersion: v.version,
          timezone: v.timezone,
          yearStartsOn: y?.startsOn ?? v.startsOn,
          yearEndsOn: y?.endsOn ?? v.endsOn,
          startsOn: v.startsOn,
          endsOn: v.endsOn,
          terms: v.terms.map(({ id: _id, ...t }) => t),
          keyDates: v.keyDates.map(({ id: _id, ...k }) => k),
          assignments: [],
          exceptions: [],
          capturedAt: now(),
        },
        proposal,
        impact: { outcome, baselineInstructionalDays: 0, proposedInstructionalDays: 0, periodShiftDays: 0, items },
        createdBy: "dev|admin@sutra.dev",
        decisionReason: null,
        decidedBy: null,
        decidedAt: null,
        version: 1,
        createdAt: now(),
        updatedAt: now(),
      };
      simulations.set(s.id, s);
      return { ...s };
    },
    async decideSimulation(id, ver, decision, reason) {
      const s = check(simulations.get(id), ver, "calendar simulation");
      if (s.status !== "evaluated") problem(409, `calendar simulation cannot move from ${s.status}`);
      return bump(s, { status: decision === "accept" ? "accepted" : "discarded", decisionReason: reason ?? null, decidedAt: now() });
    },
  };
  return api;
}
