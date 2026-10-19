# SutraERP Part 1 — full-stack demo report

**Result: PASS — human approval mode, 0 known defects.** The whole stack came up with Docker Compose in
**Keycloak / OIDC** mode with **`SUTRA_APPROVAL_MODE=human`** (the compose default since DEF-X-001), a synthetic
institution was seeded through the API, one student went **Admission → Enrolment → Attendance → Fees →
Examination → Result** (browser where a screen exists), every step was checked in PostgreSQL, the
`api` and `postgres` containers were restarted and attendance, fees and results were re-checked, and a
SQL integrity check ran against the database (0 violations, `DATABASE_INTEGRITY_REPORT.md`). Every
approval was a **real decision** by an independent checker persona; the maker's own decision was
refused with 403 for admission, fee adjustment, result approval and result publication.

| | |
|---|---|
| Branch / commit | `part1/s2-int` @ `7c26665f` (s2-demo + s2-ops merged onto the Stage 2 integration branch) |
| Demo workflow run | [38755333716](https://github.com/Varad1614/ERP_hark_2/actions/runs/38755333716) — *SutraERP Part 1 demo (full stack)*, job “Full stack demo (compose · keycloak)”: **success** |
| SutraERP CI | see PART1_EXECUTION_TRACKER.md (S2-int phase B row) for the run on the final head |
| Where | GitHub-hosted `ubuntu-24.04` runner (the development machine cannot run Docker) |
| Journey phase | 29 steps: **29 pass**, 0 fail, 0 known defects (`E2E_KNOWN_DEFECTS` empty) |
| Persistence phase | 6 steps: **6 pass**, 0 fail |
| DB integrity (after persistence) | **0 violations** — 92/92 migrations in order, 700 FKs validated + scanned, balances, duplicates, idempotency, maker-checker, 7 refused negative probes |
| Approval decisions | 18 by the journey (`approve()`) + 5 institution set-up decisions (harness); 4 maker self-decisions refused with 403 |
| Artifacts | `part1-demo-logs` (every service log, `compose ps`, resolved compose file, health output before/after the restart, restart addresses, `journey-results.json`, `db-integrity.txt`), `part1-demo-screenshots` |

How to run it yourself: [`DEMO_RUN_GUIDE.md`](DEMO_RUN_GUIDE.md). Step-by-step evidence:
[`E2E_TEST_REPORT.md`](E2E_TEST_REPORT.md).

## 1. Configuration validated before start-up

* `docker compose config` — alone, every profile, and with `docker-compose.demo.yml` (the CI override
  that copies the runner-built `sutra-api` binary into the distroless runtime image).
* `.env.example` parses completely and is a valid development configuration:
  `SUTRA_ENV_FILE=.env.example sutra-api check-config` (DEF-P1-DEMO-002 fixed — it used to stop
  parsing at line 152 and the API silently started half-configured).
* The api service's own resolved environment, in its container: `configuration ok: env=Development auth=Keycloak mfa_step_up=true http_addr=0.0.0.0:8080 session_cookie_secure=true public_url=http://localhost:3000`

## 2. Stack up — every service healthy

| Service | Image | Status |
|---|---|---|
| api | `sutraerp-api:demo` | Up 39 seconds |
| keycloak | `quay.io/keycloak/keycloak:26.7` | Up 41 seconds (healthy) |
| mailpit | `axllent/mailpit:v1.31` | Up 47 seconds (healthy) |
| migrate | `sutraerp-api:demo` | Exited (0) 39 seconds ago |
| nats | `nats:2.11-alpine` | Up 47 seconds (healthy) |
| postgres | `postgres:18` | Up 47 seconds (healthy) |
| valkey | `valkey/valkey:8` | Up 47 seconds (healthy) |
| web | `sutraerp-web` | Up 39 seconds |

| Health endpoint / check | Result |
|---|---|
| `http://localhost:8080/health/live` | 200 |
| `http://localhost:8080/health/ready` | 200 |
| `http://localhost:3000/api/v1/auth/csrf` | 200 |
| `http://localhost:8222/healthz?js-enabled-only=true` | 200 |
| `http://localhost:8025/readyz` | 200 |
| `http://localhost:8180/realms/sutra/.well-known/openid-configuration` | 2xx (body longer than the 400-byte log excerpt; `demo-wait.sh` requires 2xx before it reports ready) |
| postgres | /var/run/postgresql:5432 - accepting connections |
| valkey | PONG |
| keycloak | healthy (management /health/ready via container healthcheck) |
| migrations applied | 92 (last 20261201000302) |

Auth mode: `SUTRA_AUTH_MODE=oidc` against the imported realm `sutra` (issuer
`http://localhost:8180/realms/sutra`); the web image is built with `VITE_AUTH_MODE=oidc`.
Sign-in for all three roles and the three checker personas (registrar, finance approver, controller
of examinations — new users in the dev realm with their realm roles) went through the real Keycloak
login (authorization code + PKCE), the administrator and the checkers with password **and TOTP** — the session carries `mfaLevel=mfa`, `amr=["pwd","otp"]`
(DEF-P1-DEMO-003: the dev realm's browser flow now emits AMR references; before, admin step-up
permissions were impossible in OIDC mode).

## 3. Seed (API only)

Same synthetic institution as before (institution JIT-…, departments CSE / MATH, programmes BTECH-CSE +
BSC-MATH, offerings CS101-T1 / MD101-T1 / EL101-T1, faculty Dr. Meera Shah on CS101-T1/A, seed students
Isha Kulkarni and Kiran Joshi admitted end to end, fee structure BTECH-Y1 ₹90,000 published, exam
session with a CS101 paper and a published timetable) — but in human mode every governed step waits:
the org blueprint, campus activation, academic structure, curriculum version and calendar version are
decided by the administrator in the harness; offerings and the faculty assignment by the administrator
(administrator-only decide permissions); the two seed admissions by the **registrar**; the fee
structure by the **finance approver**; the exam policy, session and timetable by the **controller of
examinations**. Isha is no longer seated with an eligibility override (that override waits for a
decision that has no endpoint in human mode, DEF-P1-DEMO-006); the office registers her for term 1 once
the window opens and seats her normally.

## 4. Journey — summary

| Stage | What happened | Driven by |
|---|---|---|
| 1 Admission | application (wizard) → submit → review → offer → accept → 2 documents → verify both → submit for approval → **admin's own decision 403 → registrar approves** → financial clearance → confirm → student created; negatives: wrong role 403 ×2, invalid transition 409, duplicate applicant **409 at intake** (DEF-C-003 fixed) | admin browser `/students/admissions` |
| 2 Enrolment | Student 360 enrol (roll CSE-26-001, context active, admission completed); registration window opened; office registers Isha (decided) and seats her; student self-registration of 3 courses → submitted → decided → registered; student enrols in CS101-T1 / A; faculty sees it in My classes | admin + student browser; window via API |
| 3 Attendance | faculty schedules + starts a session, quick-grid marks Isha absent / Aarav present, submits → finalised; student sees 1 / 1; negatives: restart finalised session 409, re-mark finalised 409, student 403 | faculty + student browser |
| 4 Fees | charge generated from BTECH-Y1 (₹90,000, published, journal balanced); counter cash ₹30,000 with receipt; bank transfer ₹10,000 with an explicit Idempotency-Key (replay = same payment + `Idempotency-Replayed: true`, different body = 422); student pays ₹50,000 through the mock gateway checkout → receipt → “You have no dues right now”; negatives: student / faculty 403; **fee adjustment** (₹5,000 concession on Kiran's charge): admin's own decision 403 → finance approver posts it (85,000 outstanding, journal balanced) | admin + student browser; adjustment via API |
| 5 Examination | bulk registration + hall tickets; conduct (start, attendance, complete); mark sheet opened; invalid marks 150 / −5 → 422, student 403; faculty enters 78 / 100 and submits; exam cell verifies → **controller of examinations approves** → finalised | admin + faculty browser; conduct, sheet open and verify/finalise via API |
| 6 Result | batch → compute → validate → **admin's own decision 403 → controller approves** → finalise → publication → authorise requested → **admin's own decision 403 → controller authorises** → publish; term result published, SGPA 8.00, CS101-T1 78.00 → grade **A**; the student's Results screen shows it | admin + student browser |

## 5. Restart and persistence

`docker compose restart postgres api`, then the same readiness wait (all services healthy again,
`migrations applied: 92`). Container addresses (`logs/restart-addresses.txt`): `before: api 172.18.0.6 ·
postgres 172.18.0.3` / `after: api 172.18.0.6 · postgres 172.18.0.3` (unchanged this time; the
address-swap case was proven in run 38750761187 — DEF-P1-DEMO-004). Keycloak signed every role in again
right after the postgres restart (DEF-P1-DEMO-005).

| Re-check | Evidence |
|---|---|
| Stack really restarted | postgres started before: 2026-10-15 14:59:29.125418+00 · now: 2026-10-15 15:02:51.002277+00<br>✓ postgres was restarted<br>✓ API reachable through the web proxy after the restart |
| Attendance | ✓ session still finalised<br>DB attendance records: 01a14015-bff9-…:absent, 01a14015-e7a9-…:present<br>✓ identical to the pre-restart snapshot<br>student: GET /api/v1/academic/me/attendance → 200; still sees 1 / 1 |
| Fees | DB charge + payments: 0.00\|90000.00\|3 — ✓ identical to the pre-restart snapshot<br>✓ student still sees no dues |
| Results | DB term result: published\|8.00\|A:78.00 — ✓ identical to the pre-restart snapshot<br>student: GET /api/v1/exams/me/results → 200 |
| DB integrity (after the restart) | `INTEGRITY_VIOLATIONS=0`; `migrations: 92 files, 92 applied` — see DATABASE_INTEGRITY_REPORT.md |

## 6. Defects found and fixed while building the demo

| ID | Fix | Regression evidence |
|---|---|---|
| DEF-P1-DEMO-002 | `.env.example` quoted sender; malformed `.env` now refuses to start | `apps/api/tests/db_p1_demo_config.rs` + workflow `check-config` |
| DEF-P1-DEMO-003 | dev realm browser flow `browser-sutra` with AMR references (pwd / otp) | journey sign-in asserts admin + checker `mfa`; seed uses step-up permissions |
| DEF-P1-DEMO-004 | nginx resolves the api upstream per request (Docker DNS) | restart step (address swap proven in run 38750761187), web→api 200 after every restart |
| DEF-P1-DEMO-005 | Keycloak pool validates idle connections (compose) | persistence phase signs in through Keycloak right after the postgres restart |
| DEF-P1-DEMO-006 | (integrator, phase B) the journey seated a seed student with an eligibility override; in human mode the override returns 409 “awaits a decision” and **no decision endpoint exists** to complete it. The journey now registers the student properly; the missing override decision endpoint is logged as an open product gap | journey step “Seed student Isha registered by the academic office …” |
| DEF-P1-DEMO-007 | (integrator, phase B) the journey and the e2e harness assumed auto-approval: `approve()` only asserted an approved state, Batch 2 set-up steps were never decided, no checker personas existed in the Keycloak realm | real decisions + 4 self-decision 403s in every demo run; realm users registrar / finance.approver / exam.controller / payroll.approver / procurement.approver |
| DEF-P1-DEMO-008 | (integrator, phase B) the demo had no database integrity gate | `deploy/ci/db-integrity.{sh,sql}` step fails the demo on any violation |

Open, not fixed here: DEF-P1-DEMO-001 (OpenAPI `LineInput` name collision, P3 — client typing only);
DEF-P1-DEMO-006's product part (no decision endpoint for section-enrolment eligibility overrides).
Full rows: `docs/readiness/tracker/demo.md`, `DEFECT_REGISTER.md`.

## 7. Gaps and dependencies (honest list)

* **API instead of a screen** in 6 journey steps (+ the whole seed and every checker decision): see
  E2E_TEST_REPORT §3.
* Offerings, faculty assignments, student registrations and the Batch 2 set-up decisions are decided by
  the administrator: only administrators hold those decide permissions and SoD is not enforced for them
  yet (APPROVAL_CONTROL_MATRIX.md follow-up). Admissions, fee structure / adjustments and all exam
  decisions go to dedicated checker roles.
* The demo runs on a CI runner, not on a developer laptop — the machine used for development has no
  Docker. The guide covers Codespaces and local Docker (RAM figures included).
