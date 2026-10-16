// Part 2 · S5-capacity: a college day against the production-mode stack (deploy/capacity/README.md).
//
//   phase "steady"  (0–10 min)  100 staff (60 faculty: timetable, rosters, attendance marking,
//                               internal-assessment marks entry; 40 office: student search,
//                               student 360, accounts, counter fee collection, reports) +
//                               up to 300 students visiting (sign-in, dashboard, timetable,
//                               attendance, results, fees, documents + a PDF download, sign-out)
//   phase "results" (10–15 min) results-day spike: 500 students sign in within a minute and read
//                               results / grade cards, staff keep working
//   phase "fees"    (15–21 min) fee-deadline spike: 400 students read dues / receipts, office staff
//                               collect counter payments three times as often
//
// Every person signs in through the real Keycloak OIDC flow (code + PKCE via the API's BFF),
// office staff with TOTP (administrator = privileged role). Each VU binds its own source address
// (k6 --local-ips) so per-client rate limits see distinct devices, as on a campus LAN.
// Requests are tagged name / kind (read|write|file|login) / phase; per-tag stats land in the
// summary JSON (thresholds below) and deploy/capacity/analyze.py applies thresholds.json per
// endpoint. Env: CAP_BASE (https://erp.<domain>), CAP_DATA (cap-data.json), CAP_PASSWORD,
// CAP_OUT, CAP_SCALE (time scale, default 1).
/* global __ENV, open */
import http from "k6/http";
import crypto from "k6/crypto";
import exec from "k6/execution";
import { check, sleep } from "k6";
import { SharedArray } from "k6/data";
import { Trend, Counter } from "k6/metrics";

const BASE = __ENV.CAP_BASE;
const PASSWORD = __ENV.CAP_PASSWORD;
const SCALE = Number(__ENV.CAP_SCALE || 1);
const TH = JSON.parse(open(__ENV.CAP_THRESHOLDS || "../thresholds.json"));
// Shared read-only data (one copy for all VUs); META is small and copied once per VU.
const STUDENTS = new SharedArray("students", () => JSON.parse(open(__ENV.CAP_DATA)).students.filter((s) => s && s.login));
const META = new SharedArray("meta", () => {
  const d = JSON.parse(open(__ENV.CAP_DATA));
  delete d.students;
  return [d];
});
const meta = META[0];
const NSTUD = STUDENTS.length;
const student = (i) => STUDENTS[i];
const randomStudent = () => STUDENTS[Math.floor(Math.random() * NSTUD)];
// Every VU id in the test is unique and below the sum of the scenario targets.
const MAX_VUS = 60 + 40 + 300 + 500 + 400;
/**
 * The student account a visiting VU signs in as. VU v owns accounts v, v + MAX_VUS, … (disjoint
 * per VU) and rotates through them, so one account never signs in from two devices at the same
 * moment. Run 38930815741 picked accounts at random: 167 results-day sign-ins of an account
 * already signing in elsewhere were refused by Keycloak's brute-force guard
 * (user_temporarily_disabled) — recorded as a finding in CAPACITY_TEST_REPORT.md.
 */
function ownStudent() {
  const v = exec.vu.idInTest - 1;
  if (v >= NSTUD) return randomStudent();
  const owned = Math.floor((NSTUD - 1 - v) / MAX_VUS) + 1;
  return STUDENTS[v + MAX_VUS * (exec.vu.iterationInInstance % owned)];
}
const divCache = {};
/** Students of division `d` (built once per VU that needs it). */
function division(d) {
  if (!divCache[d]) {
    const out = [];
    for (let i = 0; i < NSTUD; i++) {
      const x = STUDENTS[i];
      if (x.div === d) out.push({ id: x.id, number: x.number });
    }
    divCache[d] = out;
  }
  return divCache[d];
}
const loginFlow = new Trend("login_flow", true);
const loginsDone = new Counter("logins");

const m = (min) => `${Math.max(1, Math.round(min * 60 * SCALE))}s`;
const T = (min) => min * 60 * 1000 * SCALE;
const PHASES = ["steady", "results", "fees"];
const phase = () => {
  const t = exec.instance.currentTestRunDuration;
  return t < T(10) ? "steady" : t < T(15) ? "results" : "fees";
};

// ------------------------------------------------------------------ names per kind (summary series)
const NAMES = {
  read: ["auth.session", "student.me", "academic.me.timetable", "academic.me.attendance", "exams.me.results", "exams.me.grade-cards", "exams.me.hall-tickets",
    "finance.me.accounts", "finance.me.charges", "finance.me.receipts", "finance.me.payment-gateway", "student.me.documents", "academic.my.timetable",
    "academic.section.enrolments", "academic.section.attendance-summary", "exams.paper.marks", "student.directory.search", "student.record",
    "finance.student-account", "finance.accounts.with-dues", "finance.payments.list", "exams.session.results", "services.reports.library", "academic.session.get"],
  write: ["academic.session.create", "academic.session.start", "academic.session.attendance", "academic.session.submit", "academic.session.complete",
    "academic.session.finalise", "exams.paper.marks.put", "finance.payment.counter", "auth.logout"],
  file: ["student.me.document.file"],
  login: ["login.start", "login.password", "login.otp"],
};

const thresholds = { checks: [`rate>${TH.checksRate}`] };
for (const p of PHASES) {
  thresholds[`http_req_failed{phase:${p}}`] = [`rate<${TH.errorRate}`];
  for (const k of ["read", "write", "file"]) thresholds[`http_req_duration{phase:${p},kind:${k}}`] = [`p(95)<${TH[k].p95}`, `p(99)<${TH[k].p99}`];
  thresholds[`login_flow{phase:${p}}`] = [`p(95)<${TH.loginFlow[p].p95}`];
  thresholds[`logins{phase:${p}}`] = ["count>=0"]; // per-phase sign-in count in the summary
  thresholds[`http_req_duration{phase:${p},kind:login}`] = ["max>=0"];
  for (const names of Object.values(NAMES))
    for (const n of names) {
      // Always-true entries: they make k6 report the per-endpoint series in the summary.
      thresholds[`http_req_duration{phase:${p},name:${n}}`] = ["max>=0"];
      thresholds[`http_reqs{phase:${p},name:${n}}`] = ["count>=0"];
      thresholds[`http_req_failed{phase:${p},name:${n}}`] = ["rate>=0"];
    }
  thresholds[`http_reqs{phase:${p}}`] = ["count>=0"];
}

export const options = {
  scenarios: {
    faculty: {
      executor: "ramping-vus", exec: "facultyDay", startVUs: 0,
      stages: [{ duration: m(2), target: 60 }, { duration: m(19), target: 60 }, { duration: m(0.5), target: 0 }],
      gracefulRampDown: "30s", gracefulStop: "60s",
    },
    office: {
      executor: "ramping-vus", exec: "officeDay", startVUs: 0,
      stages: [{ duration: m(2), target: 40 }, { duration: m(19), target: 40 }, { duration: m(0.5), target: 0 }],
      gracefulRampDown: "30s", gracefulStop: "60s",
    },
    students_steady: {
      executor: "ramping-vus", exec: "studentVisit", startVUs: 0, startTime: m(1),
      stages: [{ duration: m(3), target: 300 }, { duration: m(5), target: 300 }, { duration: m(1), target: 0 }],
      gracefulRampDown: "30s", gracefulStop: "30s", env: { VISIT: "day" },
    },
    results_day: {
      executor: "ramping-vus", exec: "studentVisit", startVUs: 0, startTime: m(10),
      stages: [{ duration: m(1), target: 500 }, { duration: m(3), target: 500 }, { duration: m(0.5), target: 0 }],
      gracefulRampDown: "30s", gracefulStop: "30s", env: { VISIT: "results" },
    },
    fee_deadline: {
      executor: "ramping-vus", exec: "studentVisit", startVUs: 0, startTime: m(15),
      stages: [{ duration: m(1), target: 400 }, { duration: m(3), target: 400 }, { duration: m(0.5), target: 0 }],
      gracefulRampDown: "30s", gracefulStop: "30s", env: { VISIT: "fees" },
    },
  },
  thresholds,
  summaryTrendStats: ["avg", "min", "med", "p(90)", "p(95)", "p(99)", "max", "count"],
  discardResponseBodies: false,
  insecureSkipTLSVerify: false,
  noConnectionReuse: false,
  userAgent: "SutraERP-capacity/k6",
};

// ------------------------------------------------------------------ helpers
const pick = (xs) => xs[Math.floor(Math.random() * xs.length)];
const think = (a, b) => sleep(a + Math.random() * (b - a));
function uuid() {
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    return (c === "x" ? r : (r & 3) | 8).toString(16);
  });
}
const unescapeHtml = (s) => s.replace(/&amp;/g, "&").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&lt;/g, "<").replace(/&gt;/g, ">");

function parseForm(html) {
  const forms = [...html.matchAll(/<form\b[^>]*\baction="([^"]+)"[^>]*>([\s\S]*?)<\/form>/gi)];
  if (!forms.length) return null;
  const f = forms.find((x) => /name="(?:password|otp|username)"/.test(x[2])) || forms[0];
  const fields = {};
  for (const input of f[2].matchAll(/<input\b[^>]*>/gi)) {
    const tag = input[0];
    const name = (tag.match(/\bname="([^"]+)"/i) || [])[1];
    if (!name) continue;
    const type = ((tag.match(/\btype="([^"]+)"/i) || [])[1] || "text").toLowerCase();
    if ((type === "checkbox" || type === "radio") && !/\bchecked\b/i.test(tag)) continue;
    fields[name] = unescapeHtml((tag.match(/\bvalue="([^"]*)"/i) || [])[1] || "");
  }
  return { action: unescapeHtml(f[1]), fields, otp: /name="otp"/.test(f[2]) };
}

/** RFC 6238 (HmacSHA1, 6 digits, 30 s) with Keycloak's raw-string key. */
function totp(secret, counter) {
  const msg = new ArrayBuffer(8);
  const v = new DataView(msg);
  v.setUint32(0, Math.floor(counter / 4294967296));
  v.setUint32(4, counter >>> 0);
  const h = crypto.hmac("sha1", secret, msg, "hex");
  const b = (i) => parseInt(h.substr(i * 2, 2), 16);
  const o = b(19) & 0xf;
  const code = ((b(o) & 0x7f) << 24) | (b(o + 1) << 16) | (b(o + 2) << 8) | b(o + 3);
  return String(code % 1000000).padStart(6, "0");
}

/** Full OIDC sign-in in a fresh cookie jar; returns { jar, csrf } or null. */
function signIn(user, otpSecret) {
  const ph = phase();
  const jar = new http.CookieJar();
  const t0 = Date.now();
  const tags = (name) => ({ jar, tags: { name, kind: "login", phase: ph } });
  let r = http.get(`${BASE}/api/v1/auth/oidc/start?returnTo=%2F`, tags("login.start"));
  let form = r.status === 200 ? parseForm(r.body) : null;
  if (!check(r, { "login: Keycloak form": () => !!form })) return null;
  r = http.post(form.action, Object.assign({}, form.fields, { username: user, password: PASSWORD }), tags("login.password"));
  if (r.status === 200 && r.url.indexOf(BASE) !== 0) {
    form = parseForm(r.body);
    if (form && form.otp && otpSecret) {
      // Wait out the rest of a window if this user's last code was this window (no reuse).
      const counter = Math.floor(Date.now() / 30000);
      r = http.post(form.action, Object.assign({}, form.fields, { otp: totp(otpSecret, counter) }), tags("login.otp"));
      check(r, { "login: OTP accepted": (x) => x.status < 400 });
    }
  }
  const s = http.get(`${BASE}/api/v1/auth/session`, { jar, tags: { name: "auth.session", kind: "read", phase: ph } });
  const ok = check(s, { "login: SutraERP session": (x) => x.status === 200 && !!x.json("csrfToken") });
  if (!ok) return null;
  loginFlow.add(Date.now() - t0, { phase: ph });
  loginsDone.add(1, { phase: ph });
  return { jar, csrf: s.json("csrfToken") };
}

function get(sess, name, p, kind = "read") {
  const r = http.get(`${BASE}${p}`, { jar: sess.jar, tags: { name, kind, phase: phase() } });
  check(r, { [`${name} 200`]: (x) => x.status === 200 });
  return r;
}
function send(sess, method, name, p, body, version) {
  const headers = { "content-type": "application/json", "x-csrf-token": sess.csrf, "idempotency-key": uuid() };
  if (version !== undefined && version !== null) headers["if-match"] = `"${version}"`;
  const r = http.request(method, `${BASE}${p}`, body === undefined ? "{}" : JSON.stringify(body), { jar: sess.jar, headers, tags: { name, kind: "write", phase: phase() } });
  check(r, { [`${name} 2xx`]: (x) => x.status >= 200 && x.status < 300 });
  return r;
}
const json = (r) => {
  try {
    return r.json();
  } catch {
    return null;
  }
};

// ------------------------------------------------------------------ students
export function studentVisit() {
  const visit = __ENV.VISIT;
  const st = ownStudent();
  const sess = signIn(st.login);
  if (!sess) return think(1, 3);
  think(1, 3);
  get(sess, "student.me", "/api/v1/student/me/student");
  if (visit === "results") {
    // Results day: straight to results, refresh a few times, open the grade card.
    for (let k = 0; k < 2 + Math.floor(Math.random() * 3); k++) {
      get(sess, "exams.me.results", "/api/v1/exams/me/results");
      think(1, 3);
    }
    get(sess, "exams.me.grade-cards", "/api/v1/exams/me/grade-cards");
    think(1, 2);
  } else if (visit === "fees") {
    get(sess, "finance.me.accounts", "/api/v1/finance/me/accounts");
    think(1, 3);
    get(sess, "finance.me.charges", "/api/v1/finance/me/charges");
    think(1, 3);
    get(sess, "finance.me.payment-gateway", "/api/v1/finance/me/payment-gateway");
    think(1, 2);
    get(sess, "finance.me.receipts", "/api/v1/finance/me/receipts");
    think(1, 3);
  } else {
    get(sess, "academic.me.timetable", "/api/v1/academic/me/timetable");
    think(2, 5);
    get(sess, "academic.me.attendance", "/api/v1/academic/me/attendance");
    think(2, 5);
    get(sess, "exams.me.results", "/api/v1/exams/me/results");
    think(2, 4);
    get(sess, "exams.me.hall-tickets", "/api/v1/exams/me/hall-tickets");
    think(1, 3);
    get(sess, "finance.me.accounts", "/api/v1/finance/me/accounts");
    think(2, 4);
    get(sess, "finance.me.receipts", "/api/v1/finance/me/receipts");
    think(1, 3);
    const docs = json(get(sess, "student.me.documents", "/api/v1/student/me/documents"));
    const items = (docs && (docs.items || docs)) || [];
    const listed = Array.isArray(items) ? items.find((d) => d && d.id) : null;
    const docId = st.document || (listed && listed.id);
    if (docId) {
      think(1, 2);
      get(sess, "student.me.document.file", `/api/v1/student/me/documents/${docId}/file`, "file");
    }
    think(2, 5);
  }
  const out = http.post(`${BASE}/api/v1/auth/logout`, "{}", { jar: sess.jar, redirects: 0, headers: { "content-type": "application/json", "x-csrf-token": sess.csrf }, tags: { name: "auth.logout", kind: "write", phase: phase() } });
  check(out, { "auth.logout ok": (x) => x.status < 400 });
  think(1, 4);
}

// ------------------------------------------------------------------ staff
const staffSession = {}; // per VU (module state is per VU in k6)
const sessionCounters = {};

function facultyCtx() {
  const data = meta;
  const fac = data.faculty[exec.vu.idInTest % data.faculty.length];
  const sections = [];
  for (const slot of Object.keys(data.sections))
    data.sections[slot].forEach((s, d) => {
      if (s.faculty === fac.login) sections.push({ slot, id: s.id, d });
    });
  return { fac, sections };
}

function faculty(sess, ctx) {
  const data = meta;
  const r = Math.random();
  if (!ctx.sections.length) return get(sess, "academic.my.timetable", "/api/v1/academic/my/timetable");
  const sec = pick(ctx.sections);
  if (r < 0.3) {
    get(sess, "academic.my.timetable", "/api/v1/academic/my/timetable");
    think(2, 5);
    get(sess, "academic.section.enrolments", `/api/v1/academic/sections/${sec.id}/enrolments?limit=100`);
  } else if (r < 0.6) {
    // Mark a lecture: schedule → start → whole-division register → submit → complete → finalise.
    const key = sec.id;
    const k = (sessionCounters[key] = (sessionCounters[key] || 0) + 1);
    const date = new Date(Date.now() - (1 + Math.floor(k / 5)) * 86400000).toISOString().slice(0, 10);
    const hour = 13 + (k % 5);
    // VUs whose ids differ by a multiple of the faculty count act as the same teacher with their
    // own counters: a per-alias start minute keeps their sessions distinct (run 38930815741:
    // ~20 % of session.create were 409 "already has a session at that date and time").
    // (VU ids stay below MAX_VUS = 1,300, so there are at most 13 aliases per teacher.)
    const alias = Math.floor((exec.vu.idInTest - 1) / data.faculty.length) % 13;
    const mm = String(alias * 4).padStart(2, "0");
    const me = String(alias * 4 + 3).padStart(2, "0");
    let s = json(send(sess, "POST", "academic.session.create", `/api/v1/academic/sections/${sec.id}/sessions`, { sessionDate: date, startsAt: `${hour}:${mm}`, endsAt: `${hour}:${me}`, topic: "Tutorial" }));
    if (!s || !s.id) return;
    s = json(send(sess, "POST", "academic.session.start", `/api/v1/academic/sessions/${s.id}/start`, {}, s.version)) || s;
    const students = division(sec.d);
    think(5, 15); // taking the register
    s = json(send(sess, "POST", "academic.session.attendance", `/api/v1/academic/sessions/${s.id}/attendance`, { records: students.map((x) => ({ studentId: x.id, mark: Math.random() < 0.88 ? "present" : "absent" })) }, s.version)) || s;
    s = json(send(sess, "POST", "academic.session.submit", `/api/v1/academic/sessions/${s.id}/submit-attendance`, {}, s.version)) || s;
    s = json(send(sess, "POST", "academic.session.complete", `/api/v1/academic/sessions/${s.id}/complete`, {}, s.version)) || s;
    send(sess, "POST", "academic.session.finalise", `/api/v1/academic/sessions/${s.id}/finalise-attendance`, {}, s.version);
  } else if (r < 0.8 && data.ia && sec.slot === "MD101") {
    // Internal-assessment marks for the own division (read → bulk PUT with each row's version).
    const marks = json(get(sess, "exams.paper.marks", `/api/v1/exams/exam-papers/${data.ia.paperId}/marks`));
    const mine = new Set(division(sec.d).map((x) => x.number));
    const rows = ((marks && marks.items) || []).filter((x) => mine.has(x.studentNumber) && !x.absent);
    think(10, 20); // typing
    if (rows.length) send(sess, "PUT", "exams.paper.marks.put", `/api/v1/exams/exam-papers/${data.ia.paperId}/marks`, { rows: rows.map((x) => ({ id: x.id, version: x.version, externalMarks: String(10 + Math.floor(Math.random() * 90)) })) });
  } else {
    get(sess, "academic.section.attendance-summary", `/api/v1/academic/sections/${sec.id}/attendance-summary`);
  }
  think(5, 15);
}

function office(sess) {
  const data = meta;
  const inst = data.institutionId;
  const rush = phase() === "fees";
  const r = Math.random();
  const officeIdx = exec.vu.idInTest % data.office.length;
  if (r < (rush ? 0.6 : 0.2)) {
    // Counter collection of ₹500 on a part-paid student from this desk's own queue.
    // part-paid students (index % 5 in 2..3) of this desk (index % desks)
    let s = null;
    for (let tries = 0; tries < 50 && !s; tries++) {
      const i = Math.floor(Math.random() * NSTUD);
      const x = student(i);
      if (x.account && x.charge && i % 5 >= 2 && i % 5 < 4 && i % data.office.length === officeIdx) s = x;
    }
    if (s) {
      get(sess, "finance.student-account", `/api/v1/finance/student-accounts/${s.account}`);
      think(3, 8);
      send(sess, "POST", "finance.payment.counter", `/api/v1/finance/student-accounts/${s.account}/payments`, { channel: "cash", amount: "500.00", allocations: [{ chargeId: s.charge, amount: "500.00" }] });
    }
  } else if (r < 0.55) {
    const s = randomStudent();
    get(sess, "student.directory.search", `/api/v1/student/institutions/${inst}/students?limit=20&q=${encodeURIComponent(s.number.slice(0, 9))}`);
    think(2, 5);
    get(sess, "student.record", `/api/v1/student/students/${s.id}`);
  } else if (r < 0.7) {
    get(sess, "finance.accounts.with-dues", `/api/v1/finance/institutions/${inst}/student-accounts?withDues=true&limit=50`);
  } else if (r < 0.8) {
    get(sess, "finance.payments.list", `/api/v1/finance/institutions/${inst}/payments?limit=50`);
  } else if (r < 0.9 && data.ese) {
    get(sess, "exams.session.results", `/api/v1/exams/exam-sessions/${data.ese.sessionId}/results`);
  } else if (r < 0.95) {
    const slot = pick(Object.keys(data.sections));
    get(sess, "academic.section.attendance-summary", `/api/v1/academic/sections/${pick(data.sections[slot]).id}/attendance-summary`);
  } else {
    get(sess, "services.reports.library", `/api/v1/services/institutions/${inst}/reports/library`);
  }
  think(rush ? 2 : 4, rush ? 6 : 12);
}

export function facultyDay() {
  if (!staffSession.s) {
    const ctx = facultyCtx();
    const s = signIn(ctx.fac.login);
    if (!s) return think(5, 10);
    staffSession.s = s;
    staffSession.ctx = ctx;
  }
  faculty(staffSession.s, staffSession.ctx);
}

export function officeDay() {
  const data = meta;
  if (!staffSession.s) {
    const id = exec.vu.idInTest;
    const o = data.office[id % data.office.length];
    // One desk = one office account; Keycloak refuses a TOTP code twice, so VUs that share an
    // account (only if VU ids collide modulo the desk count) sign in in different 30 s windows.
    const slot = Math.floor(id / data.office.length) % 4;
    for (let k = 0; k < 8; k++) {
      const w = Math.floor(Date.now() / 30000);
      if (w % 4 === slot && 30000 - (Date.now() % 30000) > 5000) break;
      sleep((30000 - (Date.now() % 30000)) / 1000 + 0.3);
    }
    const s = signIn(o.login, o.otp);
    if (!s) return think(30, 35);
    staffSession.s = s;
  }
  office(staffSession.s);
}

export function handleSummary(d) {
  const out = __ENV.CAP_OUT || ".";
  return {
    [`${out}/k6-summary.json`]: JSON.stringify(d, null, 1),
    stdout: `k6 done: ${d.metrics.http_reqs ? d.metrics.http_reqs.values.count : 0} requests, failed ${d.metrics.http_req_failed ? (100 * d.metrics.http_req_failed.values.rate).toFixed(2) : "?"} %\n`,
  };
}
