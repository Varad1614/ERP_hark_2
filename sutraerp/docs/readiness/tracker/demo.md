# Part 1 · Stage 2 · demo — defect tracker

Branch `part1/s2-demo`. Defects found while building and running the full-stack demonstration
(`.github/workflows/sutraerp-part1-demo.yml`, `apps/web/e2e/journey.e2e.mjs`). P0/P1 functional
and small → fixed here with a regression test; otherwise listed with severity.

| ID | Issue | Root cause | Files changed | Status | Tests | CI result + run id | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| DEF-P1-DEMO-002 | `.env.example` (the documented "copy to .env" file) is not a valid dotenv file; with it as `.env` the API silently starts with every variable after line 152 unset | `SUTRA_MAIL_FROM=SutraERP <no-reply@sutra.dev>` unquoted (dotenvy stops at the space); `main.rs` discarded the `dotenvy::dotenv()` error | `.env.example`, `apps/api/src/main.rs` (malformed `.env` now refuses to start; missing `.env` still fine) | fixed | `apps/api/tests/db_p1_demo_config.rs` (`env_example_parses_completely`, `env_example_is_a_valid_development_configuration`); demo workflow runs `sutra-api check-config` with `SUTRA_ENV_FILE=.env.example` | pending | — | pending |
| DEF-P1-DEMO-001 | OpenAPI component `LineInput` is defined three times (finance fee-structure line, student registration line, exams grading line); the generated `openapi.json` / TS client describe `POST /finance/institutions/{id}/fee-structures` `lines` as `{curriculumComponentId, kind}` instead of `{feeHeadId, description, amount}` | utoipa `ToSchema` names collide across modules (last registration wins) | — | open (P3: client typing/docs; runtime unaffected) | — | — | needs `#[schema(as = …)]` renames + `openapi.json`/`schema.d.ts` regeneration (openapi-drift job) | — |
