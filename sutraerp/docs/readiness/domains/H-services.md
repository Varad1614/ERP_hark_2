# Domain H — Institutional services & intelligence: readiness inventory (Part 1 · Stage 1)

Scope: `modules/services` (metrics directory, H07 dashboards/widgets/reports, H08 regulatory reporting,
H09 accreditation), plus the H02 notification and H03 search capabilities, which live in `modules/platform`
(`inbox/`, `search/`). Also the workspace catalogue (`catalog/workspaces.json`, `routes/workspace.tsx`) and every
L2 workflow in `architecture/L2_Domain_Workflow_Architecture/section-h` (H00–H10). Base `part1/readiness` =
`erp_merge` fb205860; CI evidence = run 38724169019 (all jobs green). Row-level data: [`H-services.csv`](H-services.csv).
Read-only review: no code was built or changed.

The method is the same as in `G-campus.md`. "browser_tested = partial" means the e2e opens the page, but
the records are seeded through the API or it's only a screenshot pass. Customer validation is `pending` everywhere.

## Summary

34 rows.

| Status | Rows |
|---|---|
| implemented | 14 |
| partial | 7 |
| missing | 6 |
| deferred | 6 (H01 ×2, H04, H05, H06, H10) |
| mock-only | 1 (workspace drawers W-H01…W-H10) |

| Release readiness | Rows |
|---|---|
| ready | 3 |
| ready-with-gaps | 14 |
| not-ready | 8 |
| deferred | 9 |

Headline: H07 governed dashboards, H08 regulatory cycles and H09 accreditation are real, persisted and
API-tested (`db_b8c` ×2, `db_b9e` ×2 passed). H08 is also browser-tested end-to-end. Gaps:
- Approvals auto-complete in production (DEF-H-001).
- Assigned dashboards are never shown to their audience (DEF-H-002).
- Search has no UI and is off by default (DEF-H-004).
- H01, H04, H05, H06 and H10 don't exist. The workspace drawer for them fakes an approval flow (DEF-H-005).

## DEFECTS

### DEF-H-001 · P1 · Dashboard, regulatory-package and accreditation approvals auto-complete in production
- Evidence: `modules/services/src/lib.rs:65` (`approvals: ApprovalMode::AutoApprove` in `ServicesDeps::into_state`).
  `apps/api/src/app.rs:873` mounts `sutra_services::router(..)` and always uses that state.
  `modules/services/src/shared/approval.rs:107` returns `Approved` immediately. `SUTRA_WORKFLOW=flowable` doesn't change this.
- Impact: a dashboard version, a regulatory submission package (`regulatory/cycles.rs:945`, `SUBMISSION_APPROVAL`),
  and accreditation frameworks, plans and readiness decisions are approved without a human. That includes the institutional
  approval before a statutory AISHE/NAAC submission (H08-W07).
- Root cause / fix: same as DEF-G-001 (systemic across modules). Derive the mode from config and test the production router.

### DEF-H-002 · P2 · Role-assigned dashboards are never shown to their assignees
- Evidence: `GET /api/v1/services/me/dashboards` (`modules/services/src/dashboards/board.rs:1435`) has no caller
  in `apps/web/app`. The Home page uses only the metrics catalogue. Admins can assign and revoke in
  `features/reports/analytics/index.tsx:265-276`, but the assignment has no visible effect.
- Fix: render `/me/dashboards` on Home (or under Reports for non-admin roles), plus an e2e step.

### DEF-H-003 · P3 · "Submitted to the regulator" is a manual reference entry, not a submission
- Evidence: `modules/services/src/regulatory/cycles.rs:1134` (`submit`) and `:1341-1373` (`acknowledge` stores a
  typed `reference`). The UI asks for "Regulator reference" (`features/reports/analytics/index.tsx:619`). There's no
  AISHE/NIRF/NAAC portal connector.
- Fix: label it "Record submission / acknowledgement" in the UI and docs, and track the connector under H10.

### DEF-H-004 · P2 · Enterprise search: off by default, three projections only, no UI
- Evidence: `modules/platform/src/search/mod.rs:108-118` (`503 search_disabled` when `SUTRA_SEARCH=off`, the default).
  Projections cover students, staff and the library catalogue only (`docs/batches.md` Batch 9 limits). `grep platform/search apps/web/app` finds nothing.
- Fix: decide the H03 scope for release. At minimum add a global search box behind the flag and index campus/services entities.

### DEF-H-005 · P2 · Workspace drawer fakes approval for unbuilt H sub-domains
- Evidence: `apps/web/app/routes/workspace.tsx:150-160` (local-state "Advance to …" + toast "moved to Approved").
  `catalog/workspaces.json` lists W-H01 (APAAR/DigiLocker), W-H04 placement, W-H05 research, W-H06 alumni and
  W-H10 external integration with 5–9 workflows each. None has a backend.
- Fix: same as DEF-G-011 (shared component).

### DEF-H-006 · P3 · Widget-retire check queries published dashboards of every tenant
- Evidence: `modules/services/src/dashboards/widgets.rs:246-249`:
  `SELECT EXISTS (… services.dashboard_version WHERE status='published' AND widgets @> …)` has no `tenant_id`.
  Widget ids are UUIDv7 and versions only reference same-institution widgets (`board.rs:596-600`), so it isn't
  exploitable. It's a full-table scan and a defense-in-depth gap. Another 20 follow-up statements in
  `modules/services/src` filter by `id` only, after a tenant-checked load (e.g. `regulatory/cycles.rs:187`, `dashboards/board.rs:295`).
- Fix: add `tenant_id = $n` to these statements, or RLS on schema `services`.

### DEF-H-007 · P2 · No segregation of duties on H07/H08/H09 decisions
- Evidence: decision handlers require `ReportApprove` (+ step-up) but never compare the decider with the
  submitter. There are 9 `ReportApprove` checks in `modules/services/src` and no submitter comparison (`grep submitted_by` finds no guard).
- Fix: refuse self-approval, as in DEF-G-004.

### DEF-H-008 · P2 · H02 notification governance missing (templates, approval, channels, scheduling)
- Evidence: `modules/platform/src/inbox/delivery.rs:1-8`: in-app + e-mail only. Bodies are hard-coded per rule
  (`modules/platform/src/inbox/rules/*.rs`). There's no template aggregate, no approval
  (L2-H02_Notification_Template_Approval_Amendment_2026-09-01) and no SMS/WhatsApp/push or scheduled sends.
- Fix: plan H02 template + channel work, or mark it deferred in `docs/batches.md`.

### DEF-H-009 · P3 · README still calls Domain H a scaffold
- Evidence: `README.md:43` (`services/ (H) … ← scaffolds`).
- Fix: update the module table.

### DEF-H-010 · P2 · Required H08/H09 workflows have no implementation
- Evidence: there's no evidence-mapping code in `modules/services/src/regulatory/` (H08-W06 Compliance Evidence Mapping).
  There's no expiry or reconciliation logic in `modules/services/src/accreditation/` (L2-H09 Compliance Requirement Expiry
  Reconciliation amendment 2026-08-27). Regulatory archival after closure is absent (H08-W12, close only).
- Fix: add to the backlog. Link regulatory values to `services.accreditation_item` / `campus.governance_evidence`.

### DEF-H-011 · P3 · Dashboard assignment roles are a hard-coded list
- Evidence: `apps/web/app/features/reports/analytics/index.tsx:32` `const ROLES = ["administrator","faculty","student"]`.
  Other roles in the role catalogue (`apps/web/app/lib/roles`) can't be targeted from the UI.
- Fix: load assignable roles from the API or role config.

## TEST GAPS
- No test asserts that production wiring leaves H07/H08/H09 decisions pending (DEF-H-001). `AwaitDecision` is covered only by
  `reports_await_decision_dashboard_and_submission_approval` and `accreditation_await_decision`, using a test-built state.
- Browser: H08 correction and closure, accreditation cycles, records, items and hand-off, dashboard rollback, assignments and
  `/me/dashboards`, the library/infrastructure operational reports — none has an e2e step. Accreditation is seeded through the API and only viewed.
- Search: `db_b9c` covers permissions/tenant scoping with a fake index. `live_opensearch.rs` is a live-only test and
  didn't run in CI run 38724169019 (not in the passed list).
- Notifications: unit tests per rule plus one DB test. There's no browser assertion that a G/H event shows up in `/notifications`.
- No negative self-approval tests (DEF-H-007).

## DOC DISCREPANCIES
- `README.md:43` calls `services/ (H)` a scaffold (DEF-H-009).
- `modules/services/src/lib.rs:4-6` says Batch 8 implements H07/H08 "as read-only projections". H09 accreditation (9E)
  writes its own aggregates and is mounted too; the header table (`lib.rs:12-15`) omits `accreditation/`.
- The `docs/batches.md` Batch 9 claim that Flowable can be "switched on per environment" doesn't hold for approvals (DEF-H-001).
- The e2e step names say "submitted to the regulator and acknowledged" (`apps/web/e2e/campus.e2e.mjs:522`). That's a manually
  typed reference (DEF-H-003).
- The user brief for this inventory lists "notifications, workspaces, analytics/search" under `modules/services`. In the code they
  live in `modules/platform` (`inbox/`, `search/`, `catalog.rs`). I inventoried them here as H02/H03 rows.
