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
| Keycloak crash loop on the demo box (realm file permissions) | The realm is copied into the image (`deploy/docker/keycloak.Dockerfile`, `COPY --chmod=0644`) instead of bind-mounted. A checkout that is not world-readable no longer stops Keycloak (UID 1000) from reading it. |

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
