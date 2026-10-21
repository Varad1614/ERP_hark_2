# Audit remediation — October 2026

One section per remediation slice. Each lists the audit findings it closes, how, and the tests.

## A1 — insecure defaults and real OIDC sign-in

Findings: **D1**, **C1**, **M10** (ERP_EXTENSIONS audit) · **SEC-2**, **SEC-7** (Teardown v2) ·
**M-3** (Final Certification) · the META SPA dev routes.

### What changed

| Finding | Fix |
|---|---|
| D1 / C1: dev password `"sutra"` built into the API (`config.rs`) | There is no default. `SUTRA_DEV_AUTH_PASSWORD` (alias `SUTRA_DEV_PASSWORD`) must be set explicitly for `SUTRA_AUTH_MODE=dev` in **every** environment: at least 12 characters and not a published dev value (`config_http::KNOWN_DEV_SECRETS`: `sutra`, `change-me`, `password`, …). A weak value fails `Config::load`. An unset value makes `serve` (`AppContext::from_config`) and `sutra-api check-config` refuse to start. Production still refuses dev auth entirely. `.env.example` ships no password. `AppContext::for_spec` (OpenAPI and in-memory tests, never serves) uses `app::SPEC_DEV_PASSWORD`. |
| C1 / M10: web `AUTH_MODE` defaulted to `mock`; Docker default `api` was the persona picker | A build without `VITE_AUTH_MODE` is `oidc`. Only `react-router dev` and Vitest (`import.meta.env.DEV`) fall back to `mock`. `web.Dockerfile` defaults to `VITE_AUTH_MODE=oidc`, `VITE_DEV_PERSONAS=false`. `oidc` shows a visible **Sign in** page (button → `GET /api/v1/auth/oidc/start?returnTo=…`). `returnTo` must be a same-origin path. `/sign-in?authError=…` callback errors are explained, and logout follows the RP-initiated `endSessionUrl`. `api` mode is a login id + password form (a real credential form for the dev authenticator). |
| D1 / SEC-7: `?? "sutra"` embedded in the bundle | Removed. The password is always typed. No `VITE_DEV_PASSWORD` is read. |
| SEC-2 / M-3: role switcher and demo login ids in every build | `lib/dev-personas.ts`: the persona picker, the account-menu role switcher and the seeded login ids exist only when `import.meta.env.DEV`, `VITE_DEV_PERSONAS=true` or `VITE_AUTH_MODE=mock`. The switcher never shows with `oidc`. In other builds the checks fold to `false` and the ids are tree-shaken out: a default build contains no `@sutra.dev` string. Form defaults that used `dev|meera.shah@sutra.dev` now use `devPersonaSubject()`. |
| SEC-7 / META: `/dev/catalogue` shipped | `routes.ts` registers the route only under `react-router dev` or with `VITE_UI_CATALOGUE=1` (CI E2E builds). Other builds emit no catalogue chunk. |
| Keycloak crash loop on the demo box (realm file permissions) | The realm is copied into the image (`deploy/docker/keycloak.Dockerfile`, `COPY --chmod=0644`) instead of bind-mounted. A checkout that is not world-readable no longer stops Keycloak (UID 1000) from reading it. The import directory is created explicitly with mode 0755 first: newer BuildKit applies `--chmod` to the parent directories COPY creates, which left `/opt/keycloak/data/import` at 0644 on the demo box (Docker 29.8 / buildx 0.37), so Keycloak imported nothing and `sutra-demo` did not exist (found during the 2026-10-21 redeploy). |

### Tests

- `apps/api/src/config.rs` → `dev_auth_refuses_without_an_explicit_strong_password`. It checks: unset is refused, `sutra`, blank and short values are refused in development and test, a strong value and the alias are accepted, and Keycloak mode ignores the value.
- `apps/api/tests/db_p1_demo_config.rs`: `.env.example` ships no dev password, and dev auth then refuses to start.
- Every integration-test context in dev auth mode sets `SUTRA_DEV_AUTH_PASSWORD = sutra_api::app::SPEC_DEV_PASSWORD` and signs in with it.
- `apps/web/app/routes/sign-in-modes.test.tsx` covers:
  - a build with no mode is `oidc`;
  - `oidc` shows a Sign in button that keeps `returnTo` and offers no personas;
  - `safeReturnTo` rejects off-site targets;
  - `api` without the flag shows only the form, with no persona ids, and sends the typed credentials;
  - `api` with `VITE_DEV_PERSONAS=true` shows the cards but still needs the typed password;
  - no client source embeds `"sutra"` or reads `VITE_DEV_PASSWORD`;
  - a built bundle, when present, has no password and no persona ids.
- `auth.test.tsx` and `sign-in.test.tsx` are updated to the typed password.
- Local run: Vitest 61 files / 403 tests passed, and `tsc` was clean. A default `react-router build` contained no `@sutra.dev`, no `"sutra"` and no catalogue chunk. A `VITE_AUTH_MODE=api VITE_DEV_PERSONAS=true VITE_UI_CATALOGUE=1` build contains both.

### CI / E2E / compose settings (what must be set)

- **CI-only dev password `ci-only-dev-signin-6d2f9b`.** It is set as `SUTRA_DEV_AUTH_PASSWORD` in the top-level `env:` of these workflows: `sutraerp-ci`, `-quality`, `-pilot`, `-data`, `-deploy`, `-observability`, `-part1-demo`, `-part2-backup`, `-part2-monitoring`, `-part2-onprem`, `-capacity`. Two workflows also need more:
  - `sutraerp-ci.yml` and `sutraerp-part1-demo.yml` also set `VITE_DEV_PERSONAS=true`, plus a transitional `VITE_DEV_PASSWORD`. That variable is now unused and can be removed.
  - The quality ZAP stack passes `-e SUTRA_DEV_AUTH_PASSWORD` to `docker run`, and part1-demo passes it through `env -i … check-config`. Part1-demo in dev mode also sets `SUTRA_COMPOSE_WEB_DEV_PERSONAS=true`.
- **Helm kind values** (`deploy/helm/sutraerp/ci/kind.yaml`): `api.extraEnv.SUTRA_DEV_AUTH_PASSWORD` uses the same value. Production renders still refuse every `SUTRA_DEV_*` key.
- **Scripts.** `deploy/ci/smoke.sh` and `check-telemetry.sh` use `SUTRA_CI_PASSWORD`, falling back to `SUTRA_DEV_AUTH_PASSWORD`. `seed-pilot.sh`, `dr-drill.sh` and `deploy/perf/{run.sh,lib.js}` use `SUTRA_DEV_AUTH_PASSWORD` (required, no default).
- **E2E.** `apps/web/e2e/lib/dev-password.mjs` holds one password per run: `E2E_PASSWORD`, else `SUTRA_DEV_AUTH_PASSWORD`, else a random local value. That value is exported to the API the suite starts. `clickDevPersona()` types it into `[data-password]` before picking a persona card. The harness, `student`, `passkey` and `lib/auth.mjs` all use it. In OIDC journeys, `lib/auth.mjs` keeps the local dev-realm password.
- **Dev compose** (`docker-compose.yml`). The api and demo-seed services pass `SUTRA_DEV_AUTH_PASSWORD` through. Export it before `docker compose --profile app up`. The web build arg `VITE_DEV_PERSONAS` comes from `SUTRA_COMPOSE_WEB_DEV_PERSONAS` (default `false`). `demo-seed` accepts `DEMO_STUDENT_SUBJECT` and `DEMO_FACULTY_SUBJECT`.

### Public demo with real sign-in (InstaBox) — not deployed by this slice

Files:
- `docker-compose.oidc-demo.yml` (override)
- `deploy/keycloak/realm-sutra-demo.json` (realm `sutra-demo`)
- `deploy/docker/keycloak.Dockerfile`
- `deploy/docker/nginx-keycloak.conf`, baked into the web image with `SUTRA_WEB_KEYCLOAK_PROXY=1`

How it fits together:
- Keycloak runs under `/auth` and is proxied by the web nginx on the same public origin. Only `/auth/realms/*` (with `master` blocked) and `/auth/resources/*` are proxied; the admin console is never exposed. The realm import resolves `${…}` placeholders from the environment.
- **Subjects.** The OIDC subject is the Keycloak `sub`, which is the user id. The demo realm fixes the user ids, so the API's subjects are predictable:

  | User | Role | Subject |
  |---|---|---|
  | `admin@sutra.dev` | administrator | `5e7a0c1e-0000-4000-8000-00000000ad01` |
  | `meera.shah@sutra.dev` | faculty | `5e7a0c1e-0000-4000-8000-00000000fa01` |
  | `aarav.desai@sutra.dev` | student | `5e7a0c1e-0000-4000-8000-000000005701` |

- **Tenant.** It comes from the `tenant_id` attribute of group `/sutra-tenant-0192f5a0-7c3e-7000-8000-000000000001`, which is the dev tenant the demo data lives in. Roles come from realm roles. The administrator must enrol TOTP at first sign-in (`CONFIGURE_TOTP`), because Keycloak mode enforces privileged MFA.
- **Data linkage.** Data seeded in dev mode is linked to `dev|<login id>` subjects. Re-link Aarav (student) and Meera (faculty assignment) to the Keycloak ids: either run `demo-seed` with `DEMO_STUDENT_SUBJECT` / `DEMO_FACULTY_SUBJECT` **before** switching (it signs in through the dev authenticator), or start from a fresh volume.

Steps on the box (repo at `/data/home/ERP_hark_2/sutraerp`, after `git pull` of ERP_EXTENSIONS):

```sh
cd /data/home/ERP_hark_2/sutraerp
export SUTRA_DEMO_PUBLIC_URL=https://3000-agqu5j43qriz5zozupkslqwlju.instabox.tech
export SUTRA_DEMO_OIDC_CLIENT_SECRET=$(openssl rand -hex 24)
export SUTRA_DEMO_KC_ADMIN_PASSWORD=$(openssl rand -hex 24)
export SUTRA_DEMO_USER_PASSWORD='<choose: >=12 chars, given to demo users>'
export SUTRA_COMPOSE_APPROVAL_MODE=auto            # as today (development demo)
# Keep these in a root-only env file (chmod 600), not in the shell history.

# 1. (Re)link the demo data to the Keycloak subjects while still on dev auth (one-off).
export SUTRA_DEV_AUTH_PASSWORD=$(openssl rand -hex 16)
docker compose --profile app up -d --build api           # dev auth needs the explicit password now
DEMO_STUDENT_SUBJECT=5e7a0c1e-0000-4000-8000-000000005701 \
DEMO_FACULTY_SUBJECT=5e7a0c1e-0000-4000-8000-00000000fa01 \
  docker compose --profile demo run --rm --build demo-seed
#    The existing DEMO institution's faculty assignment / seat rows keep the old dev subject.
#    Seats are copied at insertion, so for a clean link start from a fresh volume
#    (`docker compose down -v`, then up + demo-seed with the overrides above).

# 2. Switch to OIDC (drop the old docker-compose.override.yml from the command: -f files only).
docker compose -f docker-compose.yml -f docker-compose.oidc-demo.yml --profile app up -d --build
docker compose -f docker-compose.yml -f docker-compose.oidc-demo.yml ps          # keycloak healthy
docker compose -f docker-compose.yml -f docker-compose.oidc-demo.yml run --rm --no-deps api check-config   # auth=Keycloak
curl -fsS "$SUTRA_DEMO_PUBLIC_URL/auth/realms/sutra-demo/.well-known/openid-configuration" | jq .issuer
#    → "$SUTRA_DEMO_PUBLIC_URL/auth/realms/sutra-demo" (must equal SUTRA_OIDC_ISSUER)
```

Then open the public URL → **Sign in** → Keycloak login (`admin@sutra.dev` plus the demo password, then TOTP enrolment) → back in SutraERP. Sign out ends the Keycloak session too.

Troubleshooting:
- Keycloak shows "HTTPS required": the edge sends no `X-Forwarded-Proto`. Set `SUTRA_DEMO_KC_SSL_REQUIRED=none`. This only works on a fresh realm import; otherwise change the setting in the realm.
- The realm import runs only when the `sutra-demo` realm does not exist yet. To change its passwords or secret later, drop the realm or the Keycloak database (`DROP DATABASE keycloak; CREATE DATABASE keycloak OWNER keycloak;`) and restart.
- The Keycloak master admin from an earlier `admin/admin` bootstrap may still exist in the old Keycloak database. It is not reachable through the proxy, but recreating the Keycloak database removes it.

### Not verified here / risks

- **Docker stack not run.** No Docker in the agent sandbox, so the Keycloak image, the `/auth` proxy, the realm placeholders and the full OIDC round-trip were not run. The API's OIDC routes (`/api/v1/auth/oidc/start|callback|backchannel-logout`, `logout` → `endSessionUrl`) are unchanged and covered by the existing Keycloak CI (part1-demo, part2-backup).
- **Rust not compiled.** `cargo check -p sutra-api --lib` could not complete in the sandbox within the time limits. CI compiles it.
- **Mock fixtures.** Mock-mode data files still contain dev subjects. They ship only inside mock-only code paths, and a default build was verified to contain no `@sutra.dev` string.

## A2 — input policy (names, contacts, dates, caps)

Findings: **SEC-1 / C-1** (stored XSS in names), **VAL-1** (free-form identity text), server side of
**VAL-2 / VAL-3 / VAL-4**, **H-2** (unbounded money / seats, unreachable enquiries).

### SEC-1 / C-1 — not reproducible as XSS; defence in depth

The reported `<img src=x onerror=…>` institution / student names do **not** execute in the web
app: every place that shows a name (role switcher, `Select` options, careers page, print views,
CSV previews) renders it through React text nodes, so the markup shows literally. This is now
pinned by Vitest (`apps/web/app/features/institution/xss.test.tsx`), which renders a markup name
through each of those surfaces and asserts that no element or handler is created.

The real defect found on the way was a **spreadsheet formula injection** sink: the financial
statement CSV export wrote cell values raw. It now goes through the shared `lib/csv` formula
guard (cells starting `=`, `+`, `-`, `@`, tab or CR are prefixed), with a test in
`features/finance/ledger/financial-statements.test.ts`.

### What changed (server side)

| Finding | Fix |
|---|---|
| VAL-1 / SEC-1: names and codes accepted any characters | New `crates/kernel/src/text.rs` (`sutra_kernel::text`). Identity text (person, institution, course / offering title, staff, enquiry and guardian names, short name, registration number, affiliated university, city) is trimmed, length-capped, single line, and refuses control / bidi-override characters and `<` `>`. Free text (notes, address, prior qualification) refuses control characters other than line breaks and tabs. Every rule returns a field-level `FieldViolation`, so callers answer the usual `422 validation-failed` with `validationErrors[]` naming the camelCase field. |
| VAL-3 / H-2: admissions without a date of birth, any phone format, malformed e-mail | Admission applications need `dateOfBirth` at intake: not in the future, age 3–100 on the institution's business date. `phone` / `guardianPhone` must be an Indian mobile (10 digits starting 6–9; optional `+91` / `91` / `0`; spaces, hyphens, dots and brackets tolerated). E-mail must be `local@domain.tld` (≤ 254 chars). Staff `workEmail` uses the same check. |
| H-2: name-only enquiries | A new enquiry needs a phone **or** an e-mail (`422`, field `phone`, code `contact-required`). |
| VAL-4: 0-seat or huge capacities | Offering / section capacity is omitted (unlimited) or 1–10 000 (`MAX_SEATS`). |
| VAL-2 / H-2: unbounded payment amounts | One payment / receipt / refund is at most ₹1 crore (`MAX_TRANSACTION_AMOUNT`, `422 amount_too_large`); amounts must still be positive. |
| Student import after the policy | The import path builds rows through the same rules (`a4ccdaaf`). |

### Tests

- Unit tests in `kernel::text`, `academic::offering::support` (capacity, titles), `finance::shared::money`
  (₹1 crore cap), `institution::domain` (rename refuses markup / NUL / newline).
- `apps/api/tests/db_a2_input_policy.rs` (db-tests): institution names, admission applications
  (names, DOB required / future / age range, Indian mobile, e-mail), enquiries (contact required,
  clean names), offering seats 1–10 000 and clean titles, staff names, payment amounts ≤ ₹1 crore.
- Existing suites checked against the policy: `db_b3a.rs` bodies now carry a DOB; the E2E, pilot,
  perf and DR-drill seeds already send DOBs and Indian mobiles (pilot seed, E2E and Kind smoke pass).
- Vitest: `xss.test.tsx`, `financial-statements.test.ts`.

## A3 — API error surface, scoping and fail-closed start-up

Findings: **H-1**, **D-1**, **D-2**, **D-3**, **M8**, **SEC-6**, **L-1** (error envelope / bodies /
paging), **SEC-3 / M-1** (workspace catalogue), **SEC-5** (foreign institution ids), **M5**, **M6**,
**M7** (fail-closed checks).

### What changed

| Finding | Fix |
|---|---|
| H-1 / SEC-6 / D-1: framework rejections were `text/plain` and echoed serde messages (field names, enum variants, UUID parse errors) even to callers without the route's permission | `crates/web/src/request_guard.rs::problem_envelope` (outermost layer inside the request id) turns every error answer that is not already `application/problem+json` into an RFC 9457 problem with a generic, schema-free text and the correlation id. Status codes are kept (400 malformed JSON / path / query, 413, 415, 422 wrong shape, 405, 500 for panics). The framework text is logged at debug under the correlation id, never returned. Field-level `validationErrors` remain only for business validation raised after the permission check. |
| D-2: body-less actions (e.g. `POST /finance/institutions/{id}/receivables`) ignored any body | `reject_unexpected_body`: routes whose OpenAPI operation has no `requestBody` accept only an empty body, `{}` or `null`; anything else is `422 unexpected-request-body`. |
| D-3 / M8 / L-1: unbounded `limit`, negative offsets, silently ignored `sort` | `page_bounds`: authenticated `GET`s need `limit` in `1..=200` (or the route's own documented ceiling), `offset` ≥ 0, `page` ≥ 1; `sort` / `order` / `orderBy` are refused. `422 invalid-query-parameter`, without echoing the value. |
| SEC-3 / M-1: `/workspaces` catalogue readable without a session | `GET /workspaces` and `/workspaces/{key}` require a session (`401` otherwise); the OpenAPI marks them `session_cookie`. |
| SEC-5: foreign or made-up institution ids answered `200 []` on list endpoints | `apps/api/src/institution_guard.rs`: for signed-in `GET`s whose route template has `institutions/{param}`, a successful answer is re-checked against Domain B's `InstitutionDirectory` under the caller's tenant; an unknown (or other-tenant) institution is `404`. It runs after the handler, so permission (`403`) and validation stay first; known ids are cached briefly. |
| M5: production started even when the RLS role probe errored or timed out | `apps/api/src/startup_checks.rs`: production refuses to start unless the probe positively confirms a role subject to row-level security; other environments keep warning. |
| M6: unauthenticated `/metrics` listener bound to all interfaces by default | `SUTRA_METRICS_ADDR` now defaults to `127.0.0.1:9464`. |
| M7: import entitlement check failed open | The data-import entitlement check fails closed (`platform::dataimport`). |

### Tests

- `crates/web/src/request_guard.rs` unit tests (`page_bounds_rules`, envelope / body guard).
- `apps/api/tests/api.rs`: `malformed_json_is_problem_json_without_field_names`,
  `bad_path_uuid_and_bad_query_are_problem_json_for_signed_in_callers`,
  `bodyless_action_rejects_a_body`, `pagination_bounds_are_enforced`, plus workspace 401 and
  institution 404 cases; `auth_hardening.rs` updated.
- `institution_guard::finds_the_institution_segment_of_the_template`,
  `startup_checks::production_refuses_unless_rls_is_confirmed`, config test asserting the metrics
  default is loopback.

### Environment note

- **`SUTRA_METRICS_ADDR=0.0.0.0:9464`** must be set wherever Prometheus scrapes the API over the
  network. Already set: Helm (`configmap.yaml` renders `0.0.0.0:<metricsPort>`, behind the metrics
  NetworkPolicy), `sutraerp-observability.yml`, `sutraerp-part2-monitoring.yml`, and the capacity
  compose `api` service (published only on host loopback). A single-host install that scrapes
  `localhost` needs nothing.
- Clients that sent bodies to body-less actions, `limit` > 200, or `sort` parameters now get `422`;
  E2E and seed scripts were adjusted accordingly.

## C3 — read scope (faculty, student sessions) and M9 tenant columns

Findings: **C3 / M-2** (faculty could read every student of the tenant; students read the full
institution and entitlement records), **M9** (two institution child tables without `tenant_id`).

### What changed

| Finding | Fix |
|---|---|
| M-2: faculty held tenant-wide `student.read` | New permission `student.read.assigned` (Faculty only; Administrator keeps `student.read`). `modules/student/src/shared/read_scope.rs` resolves the caller's scope: `student.read` = every student of the tenant; `student.read.assigned` = only students holding an `enrolled` seat in a section where the caller has an **active** faculty assignment (`TaughtStudentsReader` port, `academic::taught_students::PgTaughtStudents`). Any other student is `404`, like a missing or foreign record; neither permission is `403` before any row is read. Ending the assignment ends the access. |
| M-2: which reads take the assigned scope | Directory, Student 360, status history, the student's registrations and progression decisions, progression policies. Admissions, documents, ledgers, guardians, scholarships, transfers, completion, registration windows and every institution-wide list stay on `student.read` (faculty `403`). The platform search `students` projection needs `student.read` (faculty `403`; the index has no section scope). |
| C3: administrative academic configuration readable with `academic.read` | Credit-mapping rules and reconciliation runs need `flexible_learning.manage`; faculty-assignment policies need `faculty_assignment.manage`. |
| C3: student sessions read the full institution and entitlement records | A student-only session (`AuthContext::is_student_only`) reads `Institution::public_subset` (administrator e-mail and registration number blanked, cached copy included) and no profile revisions. The entitlement is `summary_only` (active modules, status, read-only flag, origin, as-of; package, add-ons, validity window, grace and version blanked). |
| M9: `org_type_blueprint_unit_type` / `org_restructure_move` protected only through their parents | Migration `20270106000100_audit_tenant_scope.sql` adds `tenant_id` + `institution_id` (backfilled from the parent, NOT NULL) so `platform.apply_tenant_rls()` enables and forces RLS on them; inserts copy both columns from the parent row. |
| Web | Student 360 hides enrolment, documents, guardians, scholarships, completion, **registration and progression** from faculty (`FACULTY_HIDDEN_360_TABS`; a hidden or unknown tab opens the overview). Registration / progression are hidden because those tabs also read options, holds, summary, ledger, breaks, transfers and ABC sync, which stay on `student.read`. |

### Tests

- `apps/api/tests/db_audit_c3_read_scope.rs`: own-section faculty reads and the 404 / 403 matrix,
  access ending with the assignment, admin-only configuration reads, student-only institution and
  entitlement subsets, M9 columns under forced RLS.
- Faculty expectations updated in `db_b3a.rs`, `db_b3b.rs`, `db_b3c.rs`, `db_p2_entitle.rs`;
  kernel `auth` tests for the permission and `is_student_only`.
- Vitest: `features/student/index.test.ts` (faculty tab set), `record/record.test.tsx`.
- E2E: `student.e2e.mjs` — faculty opening a student they do not teach sees "Student not found"
  (the 404s are expected in the problem filter); `journey.e2e.mjs` — faculty `403` on an admission,
  `404` on Aarav's Student 360 before he is seated, `200` after the CS101 enrolment step.

## Phase B — input validation, remaining items (D-2 mass assignment, web side of VAL-2..4 / H-2 / L-1)

Most of Phase B's server work landed in **A2** (text policy, DOB / Indian mobile / e-mail, seat and
amount caps) and **A3** (pagination bounds, body-less action guard). Unknown `sort` / `order` /
`orderBy` / `sortBy` query parameters were already refused by A3's `page_bounds` with
`422 invalid-query-parameter` and a `validationErrors[]` entry naming the parameter (no list endpoint
supports caller-chosen sorting), so nothing further was needed there.

### What changed

| Finding | Fix |
|---|---|
| D-2 / mass assignment: some command bodies silently dropped unknown members | `#[serde(deny_unknown_fields)]` on the remaining payment and offering command DTOs: `RecordPaymentRequest`, `AllocateRequest`, `ReasonRequest`, `RecoverRequest`, `InitiatePaymentRequest`, `RecordUnidentifiedRequest`, `IdentifyRequest`, `CreateCollectionRequest`, `CollectionItemInput`, `AllocationInput`, `CreateOffering`, `UpdateOffering`, `UpdateOfferingSection`, `OfferingCapacityChange`, `SectionSeatRequest`, `SeatTransferRequest`, `OfferingWaitlistJoin`, `OfferingWaitlistPromote`, `SelfEnrol`, `SelfWaitlist` (admission / enquiry / student record DTOs already had it). Unknown members → `422` problem, without echoing the member name back (A3 envelope). OpenAPI marks those schemas `additionalProperties: false`, the same output utoipa generates. Left out on purpose: `ConfirmPaymentRequest` (the gateway callback can carry extra members) and `NewOfferingSection` (also serialised and stored). |
| VAL-2..4 / H-2 / L-1 web: free-form numeric inputs, errors only on submit, 422 shown as "Request failed (422)" | New shared form layer `apps/web/app/lib/forms.ts`: `LIMITS` copy the server caps (seats 1–10 000, ₹1 crore per payment, applicant age 3–100); field `rule`s (`amount`, `seats`, `mobile`, `email`, `dob`, `integer`) give native `type` / `min` / `max` / `step` / `inputMode` and run on blur and before submit. `problemFieldErrors` maps a problem's `validationErrors[]` (or `errors[]`), by `field` or JSON `pointer` (`/body/x`, `#/x`, `a[0].b`, snake_case), onto the form's fields (`member` aliases, e.g. `fullName` ← `givenName` / `familyName`, `sectionCapacity` ← `capacity`). The first invalid control gets focus. `problemMessage` shows the problem `detail` or a status sentence. `ApiProblem` in `@sutra/api-client` no longer uses a client-made "Request failed (n)" title as its message. |
| Where it applies | The four shared form components (institution `QuickForm`, academic `useAsk` / `AskForm`, finance dialog, student `SpecForm`) use `useValidatedForm`, so every dialog built on them gets on-blur checks, problem → field mapping and focus. Rules are set on the main forms: payment record, unidentified receipt, bulk collection and refund amounts (`amount`); offering section seats, add section and capacity change (`seats`); new admission dialog (DOB now required as on the server, `dob` / `mobile` / `email`; a 422 returns to the step with the field); admissions-funnel enquiry (mobile / e-mail / intake / fee, plus "a mobile or an e-mail", same rule as the server); My profile (problem mapping + focus + on-blur). |

### Tests

- `apps/api/tests/db_b_strict_bodies.rs` (db-tests): an unknown member on record payment and on
  offering create gets a 422 problem that does not echo the member; the clean body still passes.
- Vitest: `app/lib/forms.test.ts` (rules, bounds, pointer / alias mapping, human messages) and
  `app/features/finance/record-payment-form.test.tsx` (amount input bounded, on-blur error, 422
  mapped to the field and focused, no "Request failed (422)").
- Ran locally: `tsc --noEmit` (web), ESLint on the changed files, and Vitest for the new suites
  plus institution, academic offering, finance, checkout, student record / screens, imports, XSS.
  Not run locally: `cargo check` / the db-tests (CI runs them).

## C1 / C2 — separation of duties fails closed; approval ledger in the owning transaction

Findings closed: **C2** (ERP_EXTENSIONS audit §3.4: SoD fail-open for unrecorded requests, guard
coverage not provably complete) and **M1** (approval ledger written outside the owning transaction).

**C1 — fail closed.** `sutra_kernel::approval::ensure_independent_decider` — the one check every
guard ends in (`sutra_persistence::approval::guard_decision*`, each module's status-history
`ensure_independent_decider`, and the direct callers in offers, timesheets, expenses, admission,
entitlements, identity roles, exam results/publication) — now refuses with **409** when no maker at
all is known (no ledger request, no status-history maker, no maker column, or the row lookup found
nothing). Previously `latest_requesters(...).unwrap_or_default()` / a missing row meant "anyone may
decide". Order: unidentified/system decider 403 → no recorded maker 409 → self-decision 403. There is
no break-glass path in the codebase (none existed; FEATURE_GAPS A-025); the remedy is to re-submit the
item for approval. Unit test: `kernel::approval::tests::a_decision_without_a_recorded_maker_fails_closed`.

Coverage: `apps/api/tests/approval_matrix.rs` now enumerates decision endpoints from the committed
OpenAPI **and the router source** (`#[utoipa::path(post, …)]` + macro-generated routes in `modules/`),
so a new decision endpoint fails CI before OpenAPI is regenerated; the path matcher also covers
`/approve-…` and `/reject`. New test `the_router_scan_finds_every_documented_decision_endpoint` keeps the
scan honest (verified locally: a probe `…/decision` endpoint with no guard and no OpenAPI entry fails
both the guard and the matrix-document tests). The widened matcher surfaced six endpoints:
`exam_revaluation_approve_marks` had **no guard** (applies revised marks) — now guarded (creator,
review completer and outcome verifier never approve; `db_b6c` asserts the verifier gets 403 and dean
approves); five `/reject` endpoints are allowlisted with reasons (a rejection grants nothing; the
grant goes through a guarded decision). APPROVAL_CONTROL_MATRIX.md + its generator updated.

**C2 — ledger in the owning transaction.** Fixed in the shared helper, no module changes:
`sutra_persistence::approval::record_request` (called by every module's `request_approval`) now
*queues* the row in a request-scoped task-local (`sutra_kernel::approval::with_request_ledger`, set by
the web auth middleware next to `with_requester`). The next `sutra_persistence::outbox::enqueue` — the
event every governed state change emits inside its own transaction — writes the queued rows in the
**same SQL statement** (`WITH approval_ledger AS (INSERT … UNNEST …) INSERT INTO platform.outbox …`).
A ledger failure fails that statement and rolls the decision back; a rolled-back decision leaves no
ledger row. Rows left queued when a request ends (a flow that emitted no event) are written by the
middleware only if the response is 2xx; on an error response they are dropped. Background work (no
request scope) writes immediately, as before. `flush_pending(&mut conn)` exists for a transaction that
emits no event. The external workflow process start itself still cannot join the DB transaction.
DB tests (`crates/persistence/tests/postgres.rs::approval_ledger`, run by CI's persistence db-tests):
commit together, rollback leaves no row, **ledger failure (transaction-local failing trigger) rolls the
decision back**, background immediate write + leftovers returned. Kernel unit test
`ledger_rows_queue_only_inside_a_request`.

Risks / follow-ups: (1) any seeded/imported pending item without a recorded maker can no longer be
decided (409) until re-submitted — E2E/demo journeys that decide seed data may need their seeds to
record the request; (2) "losing concurrent submissions count as makers" (rest of M1) is unchanged —
it only over-restricts; (3) if a request's first outbox event after `request_approval` is in a
different transaction than the owning one, the rows ride with that transaction.

### C1/C2 CI follow-up (2026-10-21)

CI run 39563394051 (head `c24d0cd`) failed `fmt` (`crates/kernel/src/money.rs`) and 19 `sutra-api`
db-tests, nearly all `409 "separation of duties: no recorded requester …"`.

**Root cause.** The maker of a governed request is taken from the task-local requester
(`sutra_kernel::approval::with_requester`), which only the session middleware
(`sutra_web::auth::authenticate`) set. Requests that reach a module router *without* that middleware
(the integration harness mounts `router_with_state` and injects the `AuthContext`) and service
functions called directly with an explicit `Actor` therefore recorded `requested_by = NULL`. Before
C1 a missing maker meant "anyone may decide", which masked it (and let those tests decide their own
submissions); with C1 failing closed every such decision became `409`. The C2 ledger queue itself was
not at fault: it writes immediately outside a request scope and with the owning event / on success
inside one. Two real defects surfaced on the way: the people **separation** decision looked the
ledger up by the case id while the approval's business key names the engagement (every separation
decision was refused), and three decision handlers ran the SoD check before "is this yours / does it
exist / is the module active" (a foreign or unknown item answered `409`/`403` instead of `404`).

**Fixes (C1 stays fail-closed, C2's same-transaction write is unchanged).**
- `sutra_web::auth::approval_scope`: a layer on every module's `router_with_state` (academic, campus,
  exams, finance, people, resources, services, student) that gives a request carrying an
  `AuthContext` its maker and request-scoped ledger queue when no requester is set yet (a
  pass-through inside `authenticate`). `authenticate` and the layer share `with_approval_scope`
  (leftover rows written only on a 2xx response).
- `sutra_kernel::approval::with_maker(actor, fut)`: a direct service call records its explicit human
  actor as maker (never a `system:*` actor; the request's subject always wins). Applied where
  integration tests call services directly: org restructure request, registration submit,
  internal-assessment finalisation, progression decision / policy submit, academic break request.
- `people_separation_decide` guards through the case's `process_instance_id`
  (`guard_decision_for_row`); `platform_import_job_decision` loads the job and checks entitlement
  before SoD; `people_leave_self_approval_decide` checks queue membership (404) before SoD.
- Tests: decisions on items the harness submitted as the same injected administrator are now made by
  an independent checker (`dev|checker@sutra.dev` / registrar / clerk as maker) in db_b2a, b3a, b3b,
  b4a, b4b, b6a, b7a, b7b, b8b, b8c, b8d, b9e — these tests self-decided, which SoD forbids; the
  expected statuses are unchanged except `decidedBy` (now the checker). Intentional maker-403 checks
  (b8c fine waiver, b8d requisition) still use the maker.

**Remaining risks.** (1) Other service functions called directly with a human actor outside the web
stack still record no maker (409 on decision) — only the call sites above use `with_maker`; production
always runs inside `authenticate`. (2) Decision handlers that run the SoD guard before their own
existence / ownership checks answer `409` for an unknown id (fixed only for imports and leave
self-approval). (3) Other aggregates whose approval business key names a different entity than the
decision path id would be refused like separations were; none other surfaced in CI.
