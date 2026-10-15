# SutraERP Part 1 — full-stack demo report

**Result: PASS.** The whole stack came up with Docker Compose in **Keycloak / OIDC** mode, a synthetic
institution was seeded through the API, one student went **Admission → Enrolment → Attendance → Fees →
Examination → Result** (browser where a screen exists), every step was checked in PostgreSQL, the
`api` and `postgres` containers were restarted and attendance, fees and results were re-checked.

| | |
|---|---|
| Branch / commit | `part1/s2-demo` @ `04674047` |
| Demo workflow run | [38750761187](https://github.com/Varad1614/ERP_hark_2/actions/runs/38750761187) — *SutraERP Part 1 demo (full stack)*, job “Full stack demo (compose · keycloak)”: **success** |
| SutraERP CI run (same branch) | [38750760063](https://github.com/Varad1614/ERP_hark_2/actions/runs/38750760063) — **success** (Rust fmt·clippy·test, Rust integration on PostgreSQL 18 + NATS + Valkey, Web typecheck·lint·test·build, OpenAPI drift, container images) |
| Where | GitHub-hosted `ubuntu-latest` runner (the development machine cannot run Docker) |
| Journey phase | 26 steps: **25 pass**, 0 fail, 1 known open defect (DEF-C-003, fixed on `part1/s2-acad`) |
| Persistence phase | 6 steps: **6 pass**, 0 fail |
| Artifacts | `part1-demo-logs` (every service log, `compose ps`, resolved compose file, health output before/after the restart, restart addresses, `journey-results.json`), `part1-demo-screenshots` (26 PNGs) |

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
| migrations applied | 84 (last 20261120000040) |

Auth mode: `SUTRA_AUTH_MODE=oidc` against the imported realm `sutra` (issuer
`http://localhost:8180/realms/sutra`); the web image is built with `VITE_AUTH_MODE=oidc`.
Sign-in for all three roles went through the real Keycloak login (authorization code + PKCE), the
administrator with password **and TOTP** — the session carries `mfaLevel=mfa`, `amr=["pwd","otp"]`
(DEF-P1-DEMO-003: the dev realm's browser flow now emits AMR references; before, admin step-up
permissions were impossible in OIDC mode).

## 3. Seed (API only)

* institution JIT-XJVR 01a13ff7-ecf7-74dc-815c-89f45fd0e7f7 active (programme BTECH-CSE, curriculum CSE-2026, year 2026-27, terms T1/T2)
* departments CSE 01a13ff7-efe0-71d5-9829-1db41a731921, MATH 01a13ff7-f00c-77c8-9d9c-07775b2c9d53 active; campus MAIN 01a13ff7-ed9c-72ed-9075-86a2fabdbf4d
* programmes BTECH-CSE 01a13ff7-edff-74cd-80ba-b1f7907ef208 + BSC-MATH 01a13ff7-f038-7039-8df1-83ff1b44e1ae (published)
* offerings CS101-T1 01a13ff7-f079-7c09-9c9b-18f16674e923, MD101-T1, EL101-T1 enrolment open; section CS101/A 01a13ff7-f07a-7cd3-b2b4-69fbc65d0801
* faculty meera.shah@sutra.dev (f49e4eb6-4e67-4ac6-9cb3-62683c5d9040) primary instructor of CS101-T1/A, assignment 01a13ff7-f202-7d32-8583-c0bc339a39f6 active
* seed student Isha Kulkarni: admission 01a13ff7-f2b8-7769-9ae3-e96ac4df925e → student 01a13ff7-f3ca-7b59-8153-ea624d2f5fc3 (S202600001) context active
* seed student Kiran Joshi: admission 01a13ff7-f454-79f1-b355-cffa2a9b46be → student 01a13ff7-f52b-7f68-9014-23f3c8b2c624 (S202600002) context active
* Isha seated in CS101-T1/A (enrolment 01a13ff7-f589-75c2-8e28-816c05296d7b, origin override)
* fee head TUITION 01a13ff7-f5a1-7c8b-8830-c545d2d12ab2; fee structure BTECH-Y1 v1 01a13ff7-f5b5-7f3a-8c98-734cd92c7bd9 published, total ₹90000.00
* exam session ESE-XJVR 01a13ff7-f672-73af-8eab-f18e03ea40d3 active; paper CS101-T1 01a13ff7-f689-70f2-8fa2-8fb7b00cfa32 (max 100, pass 40 %); timetable 01a13ff7-f716-786f-a11e-df80abe4c904 published, sitting 01a13ff7-f734-776e-9a1a-e90959afb545 on 2026-11-09 in HALL-A

## 4. Journey — summary

| Stage | What happened | Driven by |
|---|---|---|
| 1 Admission | application (wizard) → submit → review → offer → accept → 2 documents → verify both → submit for approval → approved → financial clearance → confirm → student created; negatives: wrong role 403 ×2, invalid transition 409, duplicate applicant (known open DEF-C-003) | admin browser `/students/admissions` |
| 2 Enrolment | Student 360 enrol (roll CSE-26-001, context active, admission completed); registration window opened; student self-registration of 3 courses → registered; student enrols in CS101-T1 / A; faculty sees it in My classes | admin + student browser; window via API |
| 3 Attendance | faculty schedules + starts a session, quick-grid marks Isha absent / Aarav present, submits → finalised; student sees 1 / 1; negatives: restart finalised session 409, re-mark finalised 409, student 403 | faculty + student browser |
| 4 Fees | charge generated from BTECH-Y1 (₹90,000, published, journal balanced); counter cash ₹30,000 with receipt; bank transfer ₹10,000 with an explicit Idempotency-Key (replay = same payment + `Idempotency-Replayed: true`, different body = 422); student pays ₹50,000 through the mock gateway checkout → receipt → “You have no dues right now”; negatives: student / faculty 403 | admin + student browser |
| 5 Examination | bulk registration + hall tickets; conduct (start, attendance, complete); mark sheet opened; invalid marks 150 / −5 → 422, student 403; faculty enters 78 / 100 and submits; exam cell verifies → approval → finalised | admin + faculty browser; conduct, sheet open and verify/finalise via API |
| 6 Result | batch → compute → validate (approved) → finalise → publication → authorise → publish; term result published, SGPA 8.00, CS101-T1 78.00 → grade **A**; the student's Results screen shows it | admin + student browser |

## 5. Restart and persistence

`docker compose restart postgres api`, then the same readiness wait. Container addresses
(`logs/restart-addresses.txt`):

```
before: api 172.18.0.6 · postgres 172.18.0.5 
after:  api 172.18.0.5 · postgres 172.18.0.6
```

In this run the api and postgres containers **swapped addresses** — exactly the case that broke the
web proxy before DEF-P1-DEMO-004 (nginx resolved `api` once at start-up → 502 for ever). The
web → api proxy followed the new address, and Keycloak (sharing postgres) signed every role in again
(DEF-P1-DEMO-005 — it used to fail the first login after a postgres restart).

| Service | Image | Status |
|---|---|---|
| api | `sutraerp-api:demo` | Up 8 seconds |
| keycloak | `quay.io/keycloak/keycloak:26.7` | Up 2 minutes (healthy) |
| mailpit | `axllent/mailpit:v1.31` | Up 2 minutes (healthy) |
| migrate | `sutraerp-api:demo` | Exited (0) 2 minutes ago |
| nats | `nats:2.11-alpine` | Up 2 minutes (healthy) |
| postgres | `postgres:18` | Up 8 seconds (healthy) |
| valkey | `valkey/valkey:8` | Up 2 minutes (healthy) |
| web | `sutraerp-web` | Up 2 minutes |

| After restart | Result |
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
| migrations applied | 84 (last 20261120000040) |

| Re-check | Evidence |
|---|---|
| Stack really restarted | postgres started before: 2026-10-15 14:27:39.969671+00 · now: 2026-10-15 14:30:11.590021+00<br>✓ postgres was restarted<br>✓ API reachable through the web proxy after the restart |
| Attendance | ✓ session still finalised (finalised)<br>DB attendance records: 01a13ff7-f3ca-7b59-8153-ea624d2f5fc3:absent,01a13ff8-159b-7aed-b2c6-d444a2f654cb:present<br>✓ identical to the pre-restart snapshot<br>student: GET /api/v1/academic/me/attendance → 200<br>student still sees 1 / 1 (104-7-student-attendance-after-restart.png) |
| Fees | DB charge + payments: 0.00|90000.00|3<br>✓ identical to the pre-restart snapshot (0.00|90000.00|3)<br>student: GET /api/v1/finance/me/accounts → 200<br>✓ student still sees no dues (You have no dues right now)<br>(105-7-student-fees-after-restart.png) |
| Results | DB term result: published|8.00|A:78.00<br>✓ identical to the pre-restart snapshot (published|8.00|A:78.00)<br>student: GET /api/v1/exams/me/results → 200<br>(106-7-student-results-after-restart.png) |

## 6. Defects found and fixed while building the demo (branch `part1/s2-demo`)

| ID | Fix | Regression evidence |
|---|---|---|
| DEF-P1-DEMO-002 | `.env.example` quoted sender; malformed `.env` now refuses to start | `apps/api/tests/db_p1_demo_config.rs` + workflow `check-config` |
| DEF-P1-DEMO-003 | dev realm browser flow `browser-sutra` with AMR references (pwd / otp) | journey sign-in asserts admin `mfa`; seed uses step-up permissions |
| DEF-P1-DEMO-004 | nginx resolves the api upstream per request (Docker DNS) | restart step: addresses swapped, web→api 200, persistence sign-ins through the proxy |
| DEF-P1-DEMO-005 | Keycloak pool validates idle connections (compose) | persistence phase signs in through Keycloak right after the postgres restart |

Open, not fixed here: DEF-P1-DEMO-001 (OpenAPI `LineInput` name collision, P3 — client typing only).
Full rows: `docs/readiness/tracker/demo.md`.

## 7. Gaps and dependencies (honest list)

* **Approvals are automatic** on this base (DEF-X-001, fixed on `part1/s2-appr`): 16 approval points
  (seed + journey) went through `approve(kind, id)` in `apps/web/e2e/lib/approvals.mjs`, which today
  only reads the record and asserts its approved state; the audit trail shows `system:auto-approval`.
  The integrator switches that one function to a real maker-checker decision.
* **API instead of a screen** in 4 journey steps (+ the whole seed): see E2E_TEST_REPORT §3.
* **Duplicate admission** is still accepted through offer (DEF-C-003, fixed on `part1/s2-acad`); the
  journey reports it as a known failure (`E2E_KNOWN_DEFECTS`).
* The demo runs on a CI runner, not on a developer laptop — the machine used for development has no
  Docker. The guide covers Codespaces and local Docker (RAM figures included).
