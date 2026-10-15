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
