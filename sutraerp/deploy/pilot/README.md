# Pilot institution (slice 9E)

`seed-pilot.mjs` seeds the pilot institution `PILOT` in the fixed pilot tenant
`0192f5a0-7c3e-7000-8000-000000009110` through the public API (reusing the e2e / demo-seed harness
`apps/web/e2e/lib/harness.mjs`): institution taken to active with campus, programme and calendar,
50 students, 3 staff with engagements, fee heads + the published fee structure `PILOT-Y1`, campus
hierarchy with library and hostel room, active library + hostel policies, 3 catalogued titles and
2 hostel beds. Idempotent (`changes=0` on a re-run; `PILOT_EXPECT_NO_CHANGES=1` enforces it).

The API must run in dev auth with `SUTRA_DEV_TENANT_ID=0192f5a0-7c3e-7000-8000-000000009110`.
CI stage: `deploy/ci/seed-pilot.sh` (compose + kind modes), proved by the workflow
`sutraerp-pilot.yml` job `Pilot seed (API + PostgreSQL)`. Runbook: `docs/runbooks/pilot-go-live.md`;
checklist: `docs/runbooks/go-live-checklist.md`.

## Reset (Part 2 · Stage 6, S6-pilot)

`reset-pilot.sh` empties the **synthetic** pilot tenant so `seed-pilot.mjs` can rebuild it from
nothing (rehearsals, UAT dry runs). One transaction: every row of the tenant in every table with a
`tenant_id` (audit chain included — it is per tenant), plus orphaned child rows of tables without
one; it verifies the tenant is empty before it commits.

    bash deploy/pilot/reset-pilot.sh --dry-run --confirm 0192f5a0-7c3e-7000-8000-000000009110
    bash deploy/pilot/reset-pilot.sh --confirm 0192f5a0-7c3e-7000-8000-000000009110
    bash deploy/ci/seed-pilot.sh        # seed again (idempotent second pass)

Guards (exit 2): `SUTRA_ENV=production`; `--confirm` must repeat the tenant id; every institution
of the tenant must be synthetic (`PILOT_RESET_INSTITUTION_CODES`, default `PILOT`); the database
role must be a superuser. Proven by `sutraerp-pilot.yml` (reset → empty → re-seed → idempotent,
plus the refusals). It is not a backup/restore tool; never point it at a real institution's tenant.
