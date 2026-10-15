/**
 * Synthetic test institution of the Part 1 journey, created ONLY through the public API (no SQL):
 * institution → `active` (harness `seedInstitution`: profile, org blueprint + root unit, main
 * campus, academic structure, programme BTECH-CSE + curriculum CSE-2026, calendar 2026-27 with
 * T1/T2, open academic year), then departments (CSE, MATH), a second programme (BSC-MATH),
 * courses = offerings CS101-T1 / MD101-T1 / EL101-T1 with section A each (draft → approved →
 * published → enrolment open), faculty (Dr. Meera Shah = the faculty login, primary instructor of
 * CS101-T1 / A), two synthetic students admitted end to end through the admission API, a fee head
 * + fee structure BTECH-Y1 (₹90,000, approved → published), and an examination setup (grading
 * scheme, policy → active, session ESE-… with a CS101 paper → active, venue, official timetable).
 *
 * Every approval goes through `approve(kind, id)` (lib/approvals.mjs): a real decision by the
 * checker persona on the pending record (human approval mode).
 */
import { seedInstitution, I } from "./harness.mjs";
import { approve } from "./approvals.mjs";

const A = "/api/v1/academic";
const S = "/api/v1/student";
const F = "/api/v1/finance";
const X = "/api/v1/exams";

/** Calendar date `n` days from today in Asia/Kolkata (the institution's zone). */
export const day = (n = 0) => {
  const d = new Date(Date.now() + n * 86400000);
  return new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Kolkata" }).format(d);
};

/** Takes an admission from application to an enrolled student through the API (seed students). */
export async function admitViaApi(admin, setup, p, ev = () => {}) {
  let app = await admin.call("POST", `${S}/institutions/${setup.id}/admission-applications`, {
    givenName: p.given,
    familyName: p.family,
    dateOfBirth: p.dob,
    email: p.email,
    phone: p.phone,
    programmeId: setup.programmeId,
    admissionTermId: setup.termIds[0],
    campusId: setup.campusId,
    subjectRef: p.subjectRef ?? null,
  });
  app = await admin.call("POST", `${S}/admission-applications/${app.id}/submit`, {}, app.version);
  app = await admin.call("POST", `${S}/admission-applications/${app.id}/review`, {}, app.version);
  app = await admin.call("POST", `${S}/admission-applications/${app.id}/offer`, { offerExpiresOn: day(30), requiredDocuments: ["identity_proof"] }, app.version);
  let adm = await admin.get(`${S}/admissions/${app.admissionId}`);
  adm = await admin.call("POST", `${S}/admissions/${adm.id}/accept`, {}, adm.version);
  const doc = await admin.call("POST", `${S}/admissions/${adm.id}/documents`, { documentType: "identity_proof", title: "Aadhaar card", fileRef: "dms://admissions/id.pdf" });
  adm = await admin.get(`${S}/admissions/${adm.id}`);
  adm = await admin.call("POST", `${S}/admissions/${adm.id}/submit-documents`, {}, adm.version);
  await admin.call("POST", `${S}/admissions/${adm.id}/documents/${doc.id}/verify`, { outcome: "verified" }, doc.version);
  adm = await admin.get(`${S}/admissions/${adm.id}`);
  adm = await admin.call("POST", `${S}/admissions/${adm.id}/submit-for-approval`, {}, adm.version);
  adm = await approve("admission", adm.id, { client: admin, step: `seed: admission of ${p.given} ${p.family}` });
  adm = await admin.call("POST", `${S}/admissions/${adm.id}/financial-clearance`, {}, adm.version);
  adm = await admin.call("POST", `${S}/admissions/${adm.id}/confirm`, {}, adm.version);
  adm = await admin.call("POST", `${S}/admissions/${adm.id}/create-student`, {}, adm.version);
  const student = await admin.get(`${S}/students/${adm.studentId}`);
  const ctx = (student.contexts ?? []).find((c) => c.id === adm.studentContextId) ?? student.currentContext;
  const enrolled = await admin.call("POST", `${S}/students/${adm.studentId}/enrol`, { contextId: ctx.id, rollNumber: p.roll, batchCode: "2026", sectionCode: "A" }, ctx.version);
  const active = (enrolled.contexts ?? []).find((c) => c.id === ctx.id);
  if (active?.status !== "active") throw new Error(`seed student ${p.given}: context ${active?.status}`);
  ev(`seed student ${p.given} ${p.family}: admission ${adm.id} → student ${adm.studentId} (${enrolled.studentNumber ?? ""}) context active`);
  return { id: adm.studentId, admissionId: adm.id, contextId: ctx.id, number: enrolled.studentNumber };
}

/** Creates the whole synthetic institution; returns every id the journey needs. */
export async function seedJourney({ admin, faculty }, ev = () => {}) {
  const suffix = Date.now().toString(36).toUpperCase().slice(-4);
  const code = `JIT-${suffix}`;
  // 1. institution → active (the Batch 2 endpoints, harness).
  const setup = await seedInstitution(admin, { code, name: `Journey Institute of Technology ${suffix}`, shortName: "JIT", domain: "jit.edu.in", city: "Pune" });
  // The harness requests activation and the administrator (Institution Activation Authority) decides it.
  await approve("institution_activation", setup.id, { client: admin, decidedEarlier: true, step: "seed: institution activation decided by the administrator (harness)" });
  ev(`institution ${code} ${setup.id} active (programme BTECH-CSE, curriculum CSE-2026, year 2026-27, terms T1/T2)`);
  const ids = { ...setup, code, suffix };

  // 2. campus + departments under the root unit.
  const campuses = await admin.get(`${I}/institutions/${setup.id}/campuses`);
  ids.campusId = (campuses.items ?? campuses).find((c) => c.code === "MAIN").id;
  const units = await admin.get(`${I}/institutions/${setup.id}/org-units`);
  const root = (units.items ?? units).find((u) => u.code === "ROOT");
  ids.departments = {};
  for (const [dcode, name] of [
    ["CSE", "Department of Computer Science and Engineering"],
    ["MATH", "Department of Mathematics"],
  ]) {
    const u = await admin.call("POST", `${I}/institutions/${setup.id}/org-units`, { unitType: "department", code: dcode, name, parentId: root.id });
    const act = await admin.call("POST", `${I}/org-units/${u.id}/activate`, {}, u.version);
    if (act.status !== "active") throw new Error(`department ${dcode}: ${act.status}`);
    ids.departments[dcode] = u.id;
  }
  ev(`departments CSE ${ids.departments.CSE}, MATH ${ids.departments.MATH} active; campus MAIN ${ids.campusId}`);

  // 3. second programme.
  let p2 = await admin.call("POST", `${I}/institutions/${setup.id}/programmes`, { code: "BSC-MATH", name: "B.Sc. Mathematics", level: "ug", degreeType: "btech", durationTerms: 4, orgUnitId: ids.departments.MATH });
  p2 = await admin.call("POST", `${I}/programmes/${p2.id}/publish`, {}, p2.version);
  ids.programme2Id = p2.id;
  ev(`programmes BTECH-CSE ${setup.programmeId} + BSC-MATH ${p2.id} (${p2.status})`);

  // 4. courses: one offering per term-1 curriculum component, section A each.
  // Curriculum version reads carry no component ids: offerings name their slot (`slotCode`) and
  // the offering response carries the resolved curriculum component.
  const cv = await admin.get(`${I}/curriculum-versions/${setup.curriculumVersionId}`);
  const comps = Object.fromEntries((cv.components ?? []).filter((c) => c.termNo === 1).map((c) => [c.slotCode, c]));
  ids.components = {};
  ids.offerings = {};
  ids.sections = {};
  for (const slot of ["CS101", "MD101", "EL101"]) {
    if (!comps[slot]) throw new Error(`curriculum component ${slot} missing (have ${Object.keys(comps).join(", ")})`);
    let o = await admin.call("POST", `${A}/institutions/${setup.id}/offerings`, {
      academicTermId: setup.termIds[0],
      curriculumVersionId: setup.curriculumVersionId,
      slotCode: slot,
      offeringCode: `${slot}-T1`,
      capacity: 60,
      campusId: ids.campusId,
      orgUnitId: slot === "MD101" ? ids.departments.MATH : ids.departments.CSE,
      sections: [{ sectionCode: "A", capacity: 60 }],
    });
    o = await admin.call("POST", `${A}/offerings/${o.id}/submit`, {}, o.version);
    o = await approve("offering", o.id, { client: admin, step: `seed: offering ${slot}-T1 submitted` });
    o = await admin.call("POST", `${A}/offerings/${o.id}/publish`, {}, o.version);
    o = await admin.call("POST", `${A}/offerings/${o.id}/open-enrolment`, {}, o.version);
    if (o.status !== "enrollment_open") throw new Error(`offering ${slot}: ${o.status}`);
    ids.offerings[slot] = o.id;
    ids.components[slot] = o.curriculumComponentId;
    ids.sections[slot] = o.sections.find((s) => s.sectionCode === "A").id;
  }
  ev(`offerings CS101-T1 ${ids.offerings.CS101}, MD101-T1, EL101-T1 enrolment open; section CS101/A ${ids.sections.CS101}`);

  // 5. faculty: the faculty login teaches CS101-T1 / A.
  let fa = await admin.call("POST", `${A}/sections/${ids.sections.CS101}/faculty-assignments`, {
    facultySubjectRef: faculty.subject,
    facultyName: faculty.session.principal.displayName || "Dr. Meera Shah",
    role: "primary_instructor",
    weeklyHours: 4,
    effectiveFrom: "2026-06-15",
  });
  fa = await admin.call("POST", `${A}/faculty-assignments/${fa.id}/submit`, {}, fa.version);
  fa = await approve("faculty_assignment", fa.id, { client: admin, step: "seed: faculty assignment submitted" });
  fa = await admin.call("POST", `${A}/faculty-assignments/${fa.id}/activate`, {}, fa.version);
  if (fa.status !== "active") throw new Error(`faculty assignment: ${fa.status}`);
  ids.facultyAssignmentId = fa.id;
  ev(`faculty ${faculty.login} (${faculty.subject}) primary instructor of CS101-T1/A, assignment ${fa.id} active`);

  // 6. synthetic students through the admission API.
  ids.isha = await admitViaApi(admin, ids, { given: "Isha", family: "Kulkarni", dob: "2007-05-21", email: `isha.${suffix.toLowerCase()}@students.jit.edu.in`, phone: "+91 98220 22334", roll: "CSE-26-002" }, ev);
  ids.kiran = await admitViaApi(admin, ids, { given: "Kiran", family: "Joshi", dob: "2007-01-09", email: `kiran.${suffix.toLowerCase()}@students.jit.edu.in`, phone: "+91 98220 33445", roll: "CSE-26-003" }, ev);
  // Isha is registered by the academic office and seated in CS101 / A once the term-1
  // registration window is open (journey step 2).

  // 7. fees: fee head + structure BTECH-Y1 (₹90,000) → approved → published.
  const head = await admin.call("POST", `${F}/institutions/${setup.id}/fee-heads`, { code: "TUITION", name: "Tuition fee", category: "tuition", refundable: false });
  let fs = await admin.call("POST", `${F}/institutions/${setup.id}/fee-structures`, {
    code: "BTECH-Y1",
    name: "B.Tech year 1",
    academicYearId: setup.yearId,
    programmeId: setup.programmeId,
    dueDate: day(45),
    lines: [{ feeHeadId: head.id, description: "Tuition fee — year 1", amount: "90000.00" }],
  });
  fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/submit`, {}, fs.version);
  fs = await approve("fee_structure", fs.id, { client: admin, step: "seed: fee structure submitted" });
  fs = await admin.call("POST", `${F}/fee-structures/${fs.id}/publish`, {}, fs.version);
  if (fs.status !== "published") throw new Error(`fee structure: ${fs.status}`);
  ids.feeHeadId = head.id;
  ids.feeStructureId = fs.id;
  ev(`fee head TUITION ${head.id}; fee structure BTECH-Y1 v${fs.versionNo} ${fs.id} published, total ₹${fs.totalAmount}`);

  // 8. examinations: scheme, policy, session + CS101 paper, venue, official timetable.
  const scheme = await admin.call("POST", `${X}/institutions/${setup.id}/grading-schemes`, {
    code: "CBCS-10",
    name: "CBCS ten-point",
    bands: [
      { grade: "O", minPercent: "90", gradePoint: "10", isPass: true },
      { grade: "A+", minPercent: "80", gradePoint: "9", isPass: true },
      { grade: "A", minPercent: "70", gradePoint: "8", isPass: true },
      { grade: "B+", minPercent: "60", gradePoint: "7", isPass: true },
      { grade: "B", minPercent: "55", gradePoint: "6", isPass: true },
      { grade: "C", minPercent: "50", gradePoint: "5", isPass: true },
      { grade: "P", minPercent: "40", gradePoint: "4", isPass: true },
      { grade: "F", minPercent: "0", gradePoint: "0", isPass: false },
    ],
  });
  let pol = await admin.call("POST", `${X}/institutions/${setup.id}/exam-policies`, { code: "EXAM-POL", name: "Examination policy 2026" });
  pol = await admin.call("POST", `${X}/exam-policies/${pol.id}/submit`, {}, pol.version);
  pol = await approve("exam_policy", pol.id, { client: admin, step: "seed: exam policy submitted" });
  if (pol.status !== "active") pol = await admin.call("POST", `${X}/exam-policies/${pol.id}/activate`, {}, pol.version);
  let ses = await admin.call("POST", `${X}/institutions/${setup.id}/exam-sessions`, {
    code: `ESE-${suffix}`,
    name: "End-semester examinations",
    academicTermId: setup.termIds[0],
    gradingSchemeId: scheme.id,
    policyId: pol.id,
    startsOn: day(20),
    endsOn: day(40),
    registrationOpensOn: day(-5),
    registrationClosesOn: day(15),
  });
  const paper = await admin.call("POST", `${X}/exam-sessions/${ses.id}/papers`, { offeringId: ids.offerings.CS101, maxMarks: "100", internalWeight: "0", passPercent: "40", durationMinutes: 180 });
  ses = await admin.get(`${X}/exam-sessions/${ses.id}`);
  ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/submit`, {}, ses.version);
  ses = await approve("exam_session", ses.id, { client: admin, step: "seed: exam session submitted" });
  if (ses.status === "approved") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedule`, {}, ses.version);
  if (ses.status === "scheduled") ses = await admin.call("POST", `${X}/exam-sessions/${ses.id}/activate`, {}, ses.version);
  if (ses.status !== "active") throw new Error(`exam session: ${ses.status}`);
  const venue = await admin.call("POST", `${X}/institutions/${setup.id}/exam-venues`, { code: "HALL-A", name: "Main Hall A", capacity: 60 });
  let sch = await admin.call("POST", `${X}/exam-sessions/${ses.id}/schedules`, { name: "End-semester timetable" });
  const sitting = await admin.call("POST", `${X}/exam-schedules/${sch.id}/sittings`, { paperId: paper.id, sitsOn: day(25), startsAt: "09:30" });
  await admin.call("POST", `${X}/exam-sittings/${sitting.id}/venues`, { venueId: venue.id });
  sch = await admin.get(`${X}/exam-schedules/${sch.id}`);
  sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/submit`, {}, sch.version);
  sch = await approve("exam_schedule", sch.id, { client: admin, step: "seed: exam timetable submitted" });
  if (sch.status !== "published") sch = await admin.call("POST", `${X}/exam-schedules/${sch.id}/publish`, {}, sch.version);
  if (sch.status !== "published") throw new Error(`exam schedule: ${sch.status}`);
  Object.assign(ids, { schemeId: scheme.id, policyId: pol.id, sessionId: ses.id, sessionCode: ses.code, paperId: paper.id, venueId: venue.id, scheduleId: sch.id, sittingId: sitting.id });
  ev(`exam session ${ses.code} ${ses.id} active; paper ${paper.paperCode} ${paper.id} (max 100, pass 40 %); timetable ${sch.id} published, sitting ${sitting.id} on ${day(25)} in HALL-A`);
  return ids;
}
