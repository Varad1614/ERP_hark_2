# SutraERP — full-stack demo: how to run it

The Part 1 demonstration brings up the **whole** stack with Docker Compose — PostgreSQL 18,
Keycloak 26 (realm `sutra` imported), Valkey, NATS JetStream, Mailpit, the one-shot `migrate`
job, the API and the web app — in **Keycloak / OIDC** mode, seeds a synthetic institution through
the API and drives one student from **Admission → Enrolment → Attendance → Fees → Examination →
Result** in Chromium, checking every step in the database, then restarts `api` + `postgres` and
checks that attendance, fees and results survived.

Pieces (all in the repo):

| Piece | Path |
|---|---|
| Workflow | `.github/workflows/sutraerp-part1-demo.yml` |
| Compose stack | `sutraerp/docker-compose.yml` (+ `docker-compose.demo.yml`, CI only) |
| Readiness wait + health evidence | `sutraerp/deploy/ci/demo-wait.sh` |
| Journey spec | `sutraerp/apps/web/e2e/journey.e2e.mjs` |
| Seed (API only) | `sutraerp/apps/web/e2e/lib/journey-seed.mjs` |
| Sign-in (OIDC + dev) | `sutraerp/apps/web/e2e/lib/auth.mjs` |
| Approval helper | `sutraerp/apps/web/e2e/lib/approvals.mjs` |
| Job summary renderer | `sutraerp/apps/web/e2e/lib/journey-report.mjs` |
| Config validation | `sutra-api check-config` (`sutraerp/apps/api/src/main.rs`) |

## 1. GitHub Actions (recommended — nothing to install)

* **Automatic:** every push to a `part1/**` branch that touches `sutraerp/**` or the workflow.
* **Manual:** *Actions → “SutraERP Part 1 demo (full stack)” → Run workflow*, pick the branch and
  `auth_mode` (`keycloak` = OIDC, the default; `dev` = the dev authenticator).
  From a terminal: `gh workflow run sutraerp-part1-demo.yml -r <branch> -f auth_mode=keycloak`.

What the job does, in order (each is a separate step in the log):

1. builds `sutra-api` on the runner (`cargo build -p sutra-api`, the same debug build as the CI e2e
   jobs, with the shared Rust cache) and copies it into the distroless runtime image
   (`deploy/docker/api-prebuilt.Dockerfile`); the web image is built by compose from
   `deploy/docker/web.Dockerfile` with `VITE_AUTH_MODE=oidc`;
2. validates `docker-compose.yml` (alone, every profile, and with the demo override),
   `.env.example` (`sutra-api check-config` with `SUTRA_ENV_FILE=.env.example`) and the api
   service's own environment (`docker compose run --rm --no-deps api check-config`);
3. `docker compose up -d`, then `deploy/ci/demo-wait.sh` waits for every container health check
   (postgres, keycloak, valkey, nats, mailpit), `migrate` exit 0, API `/health/ready`, the web
   container and its `/api` proxy, the Keycloak realm — and logs `docker compose ps` plus every
   health endpoint;
4. `journey.e2e.mjs` (phase `journey`): sign-in of the three roles through Keycloak, the seed, the
   six journey stages with negatives;
5. `docker compose restart postgres api`, wait again, `journey.e2e.mjs` (phase `persistence`);
6. job summary (step table with evidence, API-only gaps, auto-approval dependencies), artifacts
   **`part1-demo-logs`** (every service log, `compose ps`, resolved compose file, health output,
   `journey-results.json`) and **`part1-demo-screenshots`**.

Typical wall time: 15–25 min (Rust build from cache ~3–8 min, image builds ~4 min, stack start
~1–2 min, journey ~5–8 min).

## 2. GitHub Codespaces

A 4-core / 16 GB codespace runs the stack comfortably (8 GB is too tight with Keycloak + the web
build). From the repository page: *Code → Codespaces → New with options → 4-core*. Then:

```bash
cd sutraerp
# Same as CI (Keycloak/OIDC); full Docker build of the API (~15–25 min the first time).
SUTRA_COMPOSE_AUTH_MODE=keycloak SUTRA_COMPOSE_WEB_AUTH_MODE=oidc docker compose --profile app up -d --build
bash deploy/ci/demo-wait.sh
# Journey (needs Node 22 + pnpm; Playwright's Chromium):
pnpm install --frozen-lockfile && pnpm --filter @sutra/web exec playwright install --with-deps chromium
E2E_AUTH=oidc E2E_BASE=http://localhost:3000 DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra \
  E2E_PHASE=journey E2E_SHOTS=/tmp/shots E2E_RESULTS=/tmp/journey.json node apps/web/e2e/journey.e2e.mjs
docker compose restart postgres api && bash deploy/ci/demo-wait.sh
E2E_AUTH=oidc E2E_PHASE=persistence E2E_SHOTS=/tmp/shots E2E_RESULTS=/tmp/journey.json node apps/web/e2e/journey.e2e.mjs
node apps/web/e2e/lib/journey-report.mjs /tmp/journey.json
```

Forward port 3000 (web) and, for sign-in from your own browser, 8180 (Keycloak). The Keycloak
issuer is fixed to `http://localhost:8180`, so open the app through the forwarded **localhost**
ports (VS Code desktop / `gh codespace ports forward 3000:3000 8180:8180`), not the
`*.app.github.dev` URLs.

To reuse a pre-built API binary instead of the Docker Rust build (much faster on small machines):
`cargo build -p sutra-api && mkdir -p deploy/docker/demo-bin && cp target/debug/sutra-api deploy/docker/demo-bin/`
then add `-f docker-compose.yml -f docker-compose.demo.yml` to every `docker compose` command (or
`export COMPOSE_FILE=docker-compose.yml:docker-compose.demo.yml`).

## 3. Locally with `docker compose`

**RAM:** the running stack needs about **3.5–4 GB** for the containers (Keycloak ≈ 1–1.2 GB,
PostgreSQL ≈ 0.3 GB, API ≈ 0.2–0.4 GB, NATS / Valkey / Mailpit / nginx ≈ 0.2 GB together) plus
≈ 0.5 GB for Chromium during the journey. **Building** the API image inside Docker needs another
3–6 GB (rustc; `CARGO_BUILD_JOBS=1` keeps it near 3 GB but takes 30–45 min) and ~10 GB of disk.
On an 8 GB laptop give Docker Desktop at least 6 GB and build with `CARGO_BUILD_JOBS=1`, or use
GitHub Actions / Codespaces. Disk: ~12 GB for images and build cache.

```bash
cd sutraerp
# dev authenticator (role cards on the sign-in page):
docker compose --profile app up -d --build
# or Keycloak / OIDC (same as CI):
SUTRA_COMPOSE_AUTH_MODE=keycloak SUTRA_COMPOSE_WEB_AUTH_MODE=oidc docker compose --profile app up -d --build
bash deploy/ci/demo-wait.sh        # waits + prints compose ps and the health endpoints
```

Windows / PowerShell: `$env:SUTRA_COMPOSE_AUTH_MODE="keycloak"; $env:SUTRA_COMPOSE_WEB_AUTH_MODE="oidc"; docker compose --profile app up -d --build`.

Open <http://localhost:3000>. Accounts (local dev realm `realm-sutra-dev.json`, password `sutra` —
local only; the public demo uses `docker-compose.oidc-demo.yml` with its own passwords, see
AUDIT_REMEDIATION_2026-10.md): `admin@sutra.dev`
(administrator; Keycloak asks to enrol TOTP on the first sign-in), `meera.shah@sutra.dev`
(faculty), `aarav.desai@sutra.dev` (student). Mail sent by the stack: <http://localhost:8025>.

Then run the journey exactly as in §2 (`E2E_AUTH=dev` for the dev authenticator). The journey
creates its own institution (`JIT-xxxx`) every run; the admin screens it drives assume that
institution is the only one (a fresh volume) — after earlier runs, `docker compose down -v` first.

Stop: `docker compose down` (keeps data) / `docker compose down -v` (wipes the volumes).

## 4. Reading the results

* **Job summary** (the run's page): one row per step — phase, step, driven by (browser / API),
  result, evidence (API responses, DB checks, audit rows, role visibility, screenshot names) — then
  the steps driven through the API and every approval that depended on auto-approval.
* **`part1-demo-logs`**: `journey-results.json` (the full evidence; render it with
  `node apps/web/e2e/lib/journey-report.mjs journey-results.json`), `health-1-startup.txt`,
  `health-2-after-restart.txt`, `restart-addresses.txt` (container addresses before / after the
  restart), `compose-ps.txt`, `compose.resolved.yml`, `api-check-config.txt` and one log per service.
* **`part1-demo-screenshots`**: numbered PNGs per stage (`04-1-admission-student-created.png` …
  `20-6-student-results.png`; `101`–`106` are the persistence phase; `fail-*` only on failures).
* Exit code: a step with status `fail` fails the job; a step whose defect id is listed in
  `E2E_KNOWN_DEFECTS` (workflow env, today `DEF-C-003`, fixed on `part1/s2-acad`) is reported as
  “known open defect” and does not fail it. Remove the id once its fix is merged.

Reports from the last green run: [`LOCAL_DEMO_REPORT.md`](LOCAL_DEMO_REPORT.md) (stack, health,
restart evidence, defects) and [`E2E_TEST_REPORT.md`](E2E_TEST_REPORT.md) (per-step table).

## 5. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `web→api(/api/v1/auth/csrf)` never ready, `/api` answers 502 after `docker compose restart api` | an old web image whose nginx resolved `api` once at start-up (DEF-P1-DEMO-004). Rebuild the web image (`docker compose build web`); `deploy/docker/nginx.conf` now resolves through Docker DNS per request. |
| The first Keycloak sign-in after `docker compose restart postgres` fails (“Invalid username or password”, Keycloak log `Database operation failed`) | Keycloak's pool kept dead connections (DEF-P1-DEMO-005). `docker-compose.yml` now sets `QUARKUS_DATASOURCE_JDBC_FOREGROUND_VALIDATION_INTERVAL=5S`; with an older compose file restart `keycloak` too. |
| Administrator actions answer `403 mfa-step-up-required` in Keycloak mode | the session has no `otp` AMR — the realm import must contain the `browser-sutra` flow (DEF-P1-DEMO-003). Wipe the Keycloak database (`docker compose down -v`) so the realm is re-imported. |
| Keycloak rejects the TOTP code | the host clock is off (TOTP is 30 s based) or the same code was used twice (`otpPolicyCodeReusable=false`); the journey waits for the next 30 s window by itself. |
| Journey admin screens show the wrong institution | earlier runs left institutions behind; start from a fresh volume (`docker compose down -v`). |
| `migrate` exited non-zero | `docker compose logs migrate`; migrations are append-only — never edit an applied one. |

## Known limits

* Approvals are automatic today (DEF-X-001, fixed on `part1/s2-appr`); the journey's approval
  steps go through `approve(kind, id)` in `apps/web/e2e/lib/approvals.mjs`, which only asserts
  the approved state and lists every dependency in the job summary.
* Steps with no screen (exam conduct) and the few driven through the API although a screen exists
  (registration window, opening the mark sheet, verify / approve / finalise marks) plus the whole seed
  are listed as gaps in the summary and in `E2E_TEST_REPORT.md`.
* The payment gateway is the dev stack's mock checkout; payment, receipt, allocation and ledger
  posting behind it are real.
* The duplicate-applicant negative is a known open defect on this base (DEF-C-003, fixed on
  `part1/s2-acad`).
* The restart does not force a new container address; Docker sometimes keeps it. The proxy fix is
  proven by runs where the addresses changed (see `restart-addresses.txt`).
