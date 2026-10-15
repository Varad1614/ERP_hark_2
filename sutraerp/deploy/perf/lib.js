// Shared k6 code of the performance gates (Batch 9 · slice 9D, docs/batch9-contracts.md §9).
//
// Mixed workload across four domains, signed in once as the dev administrator (dev auth,
// `POST /api/v1/auth/login` with the signed login-CSRF double submit, then the session's
// synchronizer CSRF token on every write):
//   reads  (70 %): student directory search + student record, academic offerings + teaching
//                  sections, finance chart of accounts + financial years, campus ticket queue +
//                  infrastructure tree, institution profile
//   writes (30 %): student guardian, academic learning provider (draft), finance fee head,
//                  campus helpdesk ticket — each with a fresh Idempotency-Key and unique code
// Every request is tagged `kind: read|write` (the §9 thresholds are per kind) and `name: <op>`
// (one time series per operation, not per URL).
//
// Inputs (env): SUTRA_PERF_API (default http://127.0.0.1:8080), SUTRA_PERF_DATA (JSON written by
// deploy/perf/seed.mjs, default ./perf-data.json), SUTRA_PERF_LOGIN (default admin@sutra.dev),
// SUTRA_PERF_OUT (directory for the summary files, default .).
/* global __ENV, __VU, __ITER, open, console */ // k6 runtime globals
import http from "k6/http";
import { check, sleep } from "k6";

export const API = __ENV.SUTRA_PERF_API || "http://127.0.0.1:8080";
const DATA = JSON.parse(open(__ENV.SUTRA_PERF_DATA || "./perf-data.json"));
const INST = DATA.institutionId;
const STUDENTS = DATA.studentIds;
const TERM = DATA.termIds[0];
const NAMES = ["Aarav", "Isha", "Kiran", "Rohan", "Meera", "Desai", "Patil", "Shah", "Iyer", "Nair"];

const pick = (xs) => xs[Math.floor(Math.random() * xs.length)];

function uuidv4() {
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    return (c === "x" ? r : (r & 0x3) | 0x8).toString(16);
  });
}

function cookieHeader(res) {
  const out = [];
  for (const name of Object.keys(res.cookies)) {
    const c = res.cookies[name];
    if (c && c.length && c[0].value) out.push(`${name}=${c[0].value}`);
  }
  return out.join("; ");
}

/** Dev sign-in: returns { cookie, csrf } shared by every VU (one administrator session). */
export function signIn() {
  const login = __ENV.SUTRA_PERF_LOGIN || "admin@sutra.dev";
  let r = http.get(`${API}/api/v1/auth/csrf`, { tags: { name: "auth.csrf", kind: "setup" } });
  if (r.status !== 200) throw new Error(`GET /auth/csrf → ${r.status}`);
  const pre = cookieHeader(r);
  const loginCsrf = r.json("csrfToken");
  r = http.post(`${API}/api/v1/auth/login`, JSON.stringify({ loginId: login, password: "sutra" }), {
    headers: { "content-type": "application/json", cookie: pre, "x-csrf-token": loginCsrf },
    tags: { name: "auth.login", kind: "setup" },
  });
  if (r.status !== 200) throw new Error(`POST /auth/login → ${r.status} ${r.body}`);
  return { cookie: cookieHeader(r) || pre, csrf: r.json("csrfToken") };
}

const READS = [
  ["student.directory", () => `/api/v1/student/institutions/${INST}/students?limit=50&q=${pick(NAMES)}`],
  ["student.record", () => `/api/v1/student/students/${pick(STUDENTS)}`],
  ["student.record", () => `/api/v1/student/students/${pick(STUDENTS)}`],
  ["academic.offerings", () => `/api/v1/academic/institutions/${INST}/offerings`],
  ["academic.sections", () => `/api/v1/academic/institutions/${INST}/teaching-sections?academicTermId=${TERM}`],
  ["finance.ledger-accounts", () => `/api/v1/finance/institutions/${INST}/ledger-accounts`],
  ["finance.financial-years", () => `/api/v1/finance/institutions/${INST}/financial-years`],
  ["campus.tickets", () => `/api/v1/campus/institutions/${INST}/tickets?limit=50&open=true`],
  ["campus.infrastructure", () => `/api/v1/campus/institutions/${INST}/infrastructure-tree`],
  ["institution.profile", () => `/api/v1/institution/institutions/${INST}`],
];

function code(prefix) {
  return `${prefix}${__VU.toString(36)}X${__ITER.toString(36)}X${Math.floor(Math.random() * 46656).toString(36)}`.toUpperCase().slice(0, 30);
}

const WRITES = [
  [
    "student.guardian.create",
    `/api/v1/student/institutions/${INST}/guardians`,
    () => ({ givenName: pick(NAMES), familyName: "Perf", phone: "+91 98765 43210", occupation: "Engineer" }),
  ],
  [
    "academic.provider.create",
    `/api/v1/academic/institutions/${INST}/learning-providers`,
    () => ({ code: code("LP"), name: "Perf learning platform", providerKind: "government_platform", modes: ["mooc"] }),
  ],
  [
    "finance.fee-head.create",
    `/api/v1/finance/institutions/${INST}/fee-heads`,
    () => ({ code: code("FH"), name: "Perf fee head", category: "library", refundable: false }),
  ],
  [
    "campus.ticket.create",
    `/api/v1/campus/institutions/${INST}/tickets`,
    () => ({ category: "facility", title: "Projector flickers in room 101", description: "k6 perf gate", isComplaint: false }),
  ],
];

/** One iteration: 70 % read, 30 % write, then a short think time. */
export function mixed(session, thinkMin = 0.05, thinkMax = 0.25) {
  if (Math.random() < 0.7) {
    const [name, path] = pick(READS);
    const r = http.get(`${API}${path()}`, { headers: { cookie: session.cookie }, tags: { name, kind: "read" } });
    check(r, { [`read ${name} 200`]: (x) => x.status === 200 });
  } else {
    const [name, path, body] = pick(WRITES);
    const r = http.post(`${API}${path}`, JSON.stringify(body()), {
      headers: {
        cookie: session.cookie,
        "content-type": "application/json",
        "x-csrf-token": session.csrf,
        "idempotency-key": uuidv4(),
      },
      tags: { name, kind: "write" },
    });
    if (r.status !== 201 && Math.random() < 0.05) console.warn(`${name} → ${r.status} ${String(r.body).slice(0, 300)}`);
    check(r, { [`write ${name} 201`]: (x) => x.status === 201 });
  }
  sleep(thinkMin + Math.random() * (thinkMax - thinkMin));
}

function trend(data, key) {
  const m = data.metrics[key];
  return m ? m.values : undefined;
}

/** handleSummary body shared by both gates: compact JSON + raw data + a human table on stdout. */
export function summary(gate, data) {
  const read = trend(data, "http_req_duration{kind:read}");
  const write = trend(data, "http_req_duration{kind:write}");
  const all = trend(data, "http_req_duration");
  const reqs = data.metrics.http_reqs ? data.metrics.http_reqs.values : {};
  const failed = data.metrics.http_req_failed ? data.metrics.http_req_failed.values.rate : null;
  const checks = data.metrics.checks ? data.metrics.checks.values.rate : null;
  const thresholds = {};
  for (const [k, m] of Object.entries(data.metrics)) if (m.thresholds) thresholds[k] = m.thresholds;
  const v = (t, k) => (t ? t[k] : null);
  const compact = {
    gate,
    finishedAt: new Date().toISOString(),
    vusMax: data.metrics.vus_max ? data.metrics.vus_max.values.max : null,
    requests: reqs.count || 0,
    rps: reqs.rate || 0,
    failedRate: failed,
    checksRate: checks,
    p95: { read: v(read, "p(95)"), write: v(write, "p(95)"), overall: v(all, "p(95)") },
    p99: { read: v(read, "p(99)"), write: v(write, "p(99)"), overall: v(all, "p(99)") },
    avg: { read: v(read, "avg"), write: v(write, "avg"), overall: v(all, "avg") },
    thresholds,
    thresholdsPassed: Object.values(thresholds).every((t) => Object.values(t).every((x) => x.ok)),
  };
  const f = (x) => (x === null || x === undefined ? "—" : Number(x).toFixed(1));
  const lines = [
    `${gate}: ${compact.requests} requests, ${f(compact.rps)} req/s, failed ${f((failed || 0) * 100)} %, checks ${f((checks || 0) * 100)} %`,
    `  p95 ms  read ${f(compact.p95.read)} · write ${f(compact.p95.write)} · overall ${f(compact.p95.overall)}`,
    `  p99 ms  read ${f(compact.p99.read)} · write ${f(compact.p99.write)} · overall ${f(compact.p99.overall)}`,
    ...Object.entries(thresholds).map(([k, t]) => `  ${Object.values(t).every((x) => x.ok) ? "✓" : "✗"} ${k} ${Object.keys(t).join(", ")}`),
    "",
  ];
  const dir = __ENV.SUTRA_PERF_OUT || ".";
  return {
    stdout: lines.join("\n"),
    [`${dir}/${gate}-summary.json`]: JSON.stringify(compact, null, 2),
    [`${dir}/${gate}-raw.json`]: JSON.stringify(data),
  };
}
