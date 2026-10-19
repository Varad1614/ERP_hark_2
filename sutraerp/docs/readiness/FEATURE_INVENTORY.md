# SutraERP feature inventory (Part 1 · Stage 1)

Base `part1/readiness` = `erp_merge` `fb205860` (CI run 38724169019, all jobs green). Domain inventories merged at `f6ffff7`.
Source of every row: the ten per-domain inventories in [`domains/`](domains/). The combined table is
[`FEATURE_INVENTORY.csv`](FEATURE_INVENTORY.csv) (one header, all rows). In the combined CSV the `domain` column is
normalised to the domain file name (e.g. `E-people`) and `api_tested` / `browser_tested` to `passed | partial | not tested`
(the domain files used slightly different spellings, e.g. `API-tested passed`). Nothing else was changed.

Definitions: **api_tested = passed** means a named Rust DB/HTTP test exercising the endpoint passed in the CI run above;
**browser_tested = passed** means an e2e spec in the CI matrix drives the flow (screenshot-only visits count as not tested);
**customer_validated** is `pending` for every row (no customer has used the product).

## Counts by status

| Domain | Rows | implemented | partial | mock-only | missing | deferred | Inventory |
|---|---:|---:|---:|---:|---:|---:|---|
| A-platform | 45 | 19 | 9 | 0 | 16 | 1 | [md](domains/A-platform.md) · [csv](domains/A-platform.csv) |
| B-institution | 43 | 37 | 5 | 0 | 0 | 1 | [md](domains/B-institution.md) · [csv](domains/B-institution.csv) |
| C-student | 50 | 30 | 18 | 2 | 0 | 0 | [md](domains/C-student.md) · [csv](domains/C-student.csv) |
| D-academic | 45 | 28 | 16 | 0 | 1 | 0 | [md](domains/D-academic.md) · [csv](domains/D-academic.csv) |
| E-people | 56 | 32 | 17 | 0 | 7 | 0 | [md](domains/E-people.md) · [csv](domains/E-people.csv) |
| F-finance | 40 | 31 | 4 | 1 | 2 | 2 | [md](domains/F-finance.md) · [csv](domains/F-finance.csv) |
| G-campus | 62 | 35 | 13 | 1 | 9 | 4 | [md](domains/G-campus.md) · [csv](domains/G-campus.csv) |
| H-services | 34 | 14 | 7 | 1 | 6 | 6 | [md](domains/H-services.md) · [csv](domains/H-services.csv) |
| I-exams | 36 | 25 | 10 | 0 | 0 | 1 | [md](domains/I-exams.md) · [csv](domains/I-exams.csv) |
| J-resources | 28 | 18 | 1 | 0 | 1 | 8 | [md](domains/J-resources.md) · [csv](domains/J-resources.csv) |
| **Total** | **439** | **269** | **100** | **5** | **42** | **23** | |

## Counts by release readiness

| Domain | ready | ready-with-gaps | not-ready | deferred |
|---|---:|---:|---:|---:|
| A-platform | 12 | 11 | 19 | 3 |
| B-institution | 17 | 23 | 2 | 1 |
| C-student | 20 | 23 | 5 | 2 |
| D-academic | 11 | 30 | 3 | 1 |
| E-people | 3 | 27 | 21 | 5 |
| F-finance | 4 | 21 | 13 | 2 |
| G-campus | 3 | 37 | 16 | 6 |
| H-services | 3 | 14 | 8 | 9 |
| I-exams | 4 | 18 | 13 | 1 |
| J-resources | 3 | 9 | 8 | 8 |
| **Total** | **80** | **213** | **108** | **38** |

Testing across all 439 rows: API-tested passed 365, partial 1, not tested 73;
browser-tested passed 146, partial 9, not tested 284. Detail: [`TEST_COVERAGE_MATRIX.md`](TEST_COVERAGE_MATRIX.md).

## Release-scope notes

- **No domain is release-ready as a whole.** The blocker shared by all of them is [DEF-X-001](DEFECT_REGISTER.md#p0): every governed
  decision (admissions, payroll, refunds, write-offs, result publication, purchase orders, ...) is approved by `system:auto-approval`
  in every environment, including `SUTRA_WORKFLOW=flowable`. Many `ready` / `ready-with-gaps` rows are rated on the assumption
  that this is fixed; the rows that depend on a human decision are mostly `partial` / `not-ready` for that reason.
- Fixing DEF-X-001 first needs the decision screens that are missing today (API-only decision endpoints in B, C, D, E, F, G, I, J;
  see [`FEATURE_GAPS.md`](FEATURE_GAPS.md)), otherwise governed workflows stall with no way to approve them.
- **Separation of duties** (DEF-X-002) does not exist: one Administrator role holds every manage + approve permission.
- **Mock-only** rows: ABC sync and external-scholarship sync (C), online payment gateway (F, mock still selectable in production,
  DEF-F-002), and the catalog workspace drawers for G and H (client-side fake approval, DEF-X-004).
- **Deferred by plan** (not release blockers if the release scope says so): G06–G09, H01, H04–H06, H10, J09/J10 and the 8E
  procurement depth, F budgets (F-033…038) and asset accounting (F-067…072), I practical/viva (6D), A password management
  (delegated to Keycloak). The release scope must state these explicitly; today `docs/batches.md` only partly names them.
- **Missing without a plan**: most L2-A governance (tenant provisioning, person/identity, role admin, delegation/SoD, workflow
  governance and human-task inbox, audit investigation/retention, master data, operational readiness), E deputation /
  availability / HR case / promotion / compliance, salary disbursement and statutory outputs (E), F accounting batches and
  fund transfers, large parts of G01/G02/G03/G05/G10, H02 templates, H03 search governance, H07 depth, H08-W06, H09 amendments,
  J01 procurement planning. Full list: [`FEATURE_GAPS.md`](FEATURE_GAPS.md).
- Customer validation is pending for every feature.

Related: [`DEFECT_REGISTER.md`](DEFECT_REGISTER.md) · [`FEATURE_GAPS.md`](FEATURE_GAPS.md) · [`TEST_COVERAGE_MATRIX.md`](TEST_COVERAGE_MATRIX.md) · [`PART1_EXECUTION_TRACKER.md`](PART1_EXECUTION_TRACKER.md)
