# Tracker — Part 2 · Stage 7 · S7-commercial

Branch `part2/s7-commercial` from `part2/s4-int` @ `fac412ce`. Docs only (no code, no CI run needed: commits carry `[skip ci]`).

| Stage | Task | Defect / feature | Changed files | Tests / evidence | Actual result | Status | Blockers | Commit |
|---|---|---|---|---|---|---|---|---|
| 7 | Read evidence: inventory, defects, gaps, E2E, security, Part 2 tracker; S5 deploy / backup / monitor reports; entitlement registry | — | — (read) | FEATURE_INVENTORY.csv (439 rows, Stage 1 baseline), DEFECT_REGISTER.md (0 P0/P1 open), E2E_TEST_REPORT.md, SECURITY_AUDIT.md, `part2/s5-*` reports, `catalog/entitlements.json` @ s7-entitle `bf48b7f6` | evidence gathered; register has stale rows (noted) | COMPLETED | — | — |
| 7 | Module catalogue (15 modules, class, evidence, open defects) | commercial | `docs/readiness/MODULE_CATALOGUE.md` | per-module inventory counts (reproducible mapping §4), e2e suites, run ids | 2 Production-ready (technically verified; institution acceptance pending): admissions, attendance · 13 Pilot-ready · 0 under development / deferred at module level | READY_FOR_REVIEW | institution acceptance BLOCKED_EXTERNAL | 60e452c3, dfccbb5 |
| 7 | Commercial packaging (4 packages, contents, dependencies, licensed functionality, add-ons) | commercial | `docs/readiness/COMMERCIAL_PACKAGING.md` | registry `catalog/entitlements.json` v1 (s7-entitle `bf48b7f6`) | first version used the fallback keys before the registry existed (c0b54ef); reconciled to the registry's cumulative tiers (03e3d8f); unencoded functional deps R1–R5 listed | READY_FOR_REVIEW | s7-entitle CI 38940451926 in progress; R1/R2 untested | c0b54ef, 03e3d8f |
| 7 | Pricing & unit economics (INR price list, market reference, 2,000-student cost/margin model) | commercial | `docs/readiness/PRICING_AND_UNIT_ECONOMICS.md` | web sources cited (UNVERIFIED); all SutraERP prices / costs ASSUMPTION | core_academic ₹150, academic_plus ₹220, institution_operations ₹270, complete_erp ₹300 per student/yr; 2,000-student complete_erp Y1 ₹12.45 L, margin 13.7 % (pilot −10.5 %), 5-yr 33.5 % | READY_FOR_REVIEW | CA (GST) + legal review; real quotes | c1b5adc, 9781787 |
| 7 | Support & maintenance (self-hosted vs managed, RACI, tiers, SLAs, patching, windows, backup, upgrade, offboarding) | commercial | `docs/readiness/SUPPORT_AND_MAINTENANCE.md` | consistent with MONITORING_AND_SUPPORT.md Sev-1..3, BACKUP_RESTORE_REPORT.md, runbook, s7-entitle licence states | proposed policy; read-only-after-expiry matches kernel `decide` | READY_FOR_REVIEW | managed hosting not drilled | 3a8cf94, dfccbb5 |
| 7 | Commercial readiness gate (no production claim) | commercial | `docs/readiness/COMMERCIAL_READINESS.md` | gate table G1–G26 | NOT production-ready; offerable as supervised design-partner pilot only | READY_FOR_REVIEW | G10, G12–G15, G18, G19 BLOCKED_EXTERNAL | 7fbb588 |
| 7 | Reconcile with s7-entitle once integrated (package text, ENTITLEMENT_IMPLEMENTATION.md, R1/R2 test outcome) | commercial | COMMERCIAL_PACKAGING.md, PRICING §3.1 | — | — | NOT_STARTED | s7-entitle not integrated | — |

## Notes

- `part2/s7-entitle` did not exist on the remote at the start of this stage; it appeared at `bf48b7f6` during the stage. The registry is
  authoritative; its packages are cumulative (core_academic ⊂ academic_plus ⊂ institution_operations ⊂ complete_erp) and `fees`
  is **not** in `core_academic` — see reconciliation items R1/R2 in COMMERCIAL_PACKAGING.md §4.2.
- Registry `defaultPackage` = `complete_erp` (`SUTRA_ENTITLEMENT_DEFAULT`): commercial installs must set `none` or store the
  purchased entitlement, otherwise every module is enabled.
- The project sandbox workspace was reset twice during this stage (work/ emptied, including `work/tools/part2-status.txt` and the
  common brief); the clone was re-created and every document had already been pushed except one, which was rewritten and pushed.
  The status file now contains only lines written after the reset.
