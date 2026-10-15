# SutraERP re-skin v2 — plan (2026-10-15)

**Directive (user, 2026-10-15):** redo the ENTIRE ERP UI to match `SutraERP-Prototype-Lite.final.html` strictly — it is
the only UI/UX source: colours, fonts, navigation, layout and every page. Keep every function and API call.

**Why v2:** the first re-skin (f3b5abc5) ported the tokens 1:1 but kept the admin-console structure. The audit
(`/workspace/work/ui-audit/gap-report.md`, 105 screens: score 3 = 3, 2 = 17, 1 = 75, 0 = 10) shows the drift is page
structure: breadcrumb subtitles instead of a live summary, 7–16-tab strips under the title, DataTables with clusters of
text buttons, no cards / approval cards / KPI sparklines / week grid / quick grid, unstyled native forms (mm/dd/yyyy,
comma text, bare checkbox lists), raw numbers (₹40645.16, 12.00 days) and wire codes.

## Slices

| Slice | Branch | Scope | Prototype screens | Depends | Est. |
|---|---|---|---|---|---|
| **R0 Foundation** | `reskin2/foundation` | `packages/ui` prototype components + formatters, `/dev/catalogue`, `reskin2` demo seed + visual-proof e2e (artifact `e2e-shots-reskin2`), contracts doc, plan / briefs / rules, CI on `reskin2/**` | catalogue | — | done |
| R1 Shell + Home + overlays | `reskin2/r1` | header / pills at 1440 + mobile; avatar menu with collapsible **Personalization** (Customize, Theme, Layouts, Focus, Print) + Install app / Take the tour / Institution branding / Switch role / Sign out; **customizable Home** (drag / resize / colour / widget library / layouts / focus / print / publish-default, theme drawer, lite); KPI sparklines from real series; palette actions; bell popover; to-dos / notices / notifications / profile / security / MFA / workspaces / workspace detail restyle | `*-home`, `adm-pz-*`, `*-menu*`, `*-palette`, `*-bell`, `*-todos*`, `*-notices*`, `*-profile`, `*-workspaces*`, `*-ws-*`, `*-dark-*`, `*-hc-home`, `*-lite-home`, `*-mobile-*`, `*-tablet-home` | R0 | L (6–8 h) |
| R2 Students + Student 360 | `reskin2/r2` | `/students` overview (search, New admission 3-step dialog, Programme / Semester / Fee chips, avatar table); 7 tabs → ≤5 + More; Student 360 (back eyebrow, Overview / Academics / Attendance / Fees / Documents, lifecycle via More / panels); student Documents + Registration | `adm-students*`, `stu-documents` | R0 | M–L (5–6 h) |
| R3 Academics + teaching | `reskin2/r3` | `/academics` (programme cards + courses table + Add course dialog), `/academic-delivery` folded to ≤5 + More; faculty Classes (cards → class detail Roster / Materials / Sessions), **Attendance quick grid**, My students, Resources, Timetable **WeekGrid**; student Timetable, Courses (+ detail), Assignments (Pending / Submitted / Graded) | `adm-academics*`, `fac-classes*`, `fac-attendance*`, `fac-students*`, `fac-timetable*`, `fac-resources*`, `stu-timetable*`, `stu-courses*`, `stu-assignments*` | R0 | L (7–8 h) |
| R4 Exams | `reskin2/r4` | `/examinations` overview (3 KPIs, DateList calendar, Result-verification approvals) + ≤5 tabs; faculty Assessments rows; student Results | `adm-examinations`, `fac-assessments`, `stu-results*` | R0 | M (4–5 h) |
| R5 Finance + Fees | `reskin2/r5` | `/finance` overview (4 money KPIs, monthly collections BarChart, fee-adjustment ApprovalCards, invoices table with status chips); 9 tabs → Overview / Collections / Receivables / Ledger + More; student Fees (KPIs, invoice table, Pay now dialog) | `adm-finance`, `stu-fees*` | R0 | M–L (5–6 h) |
| R6 People + Leave | `reskin2/r6` | `/people` (dept chips + search + PersonCards, tabs Directory · n / Leave requests · n); recruitment / positions / attendance / workload / payroll behind More; faculty Leave = prototype page (3 Balance tiles + inline apply form + history) | `adm-people*`, `fac-leave` | R0 | M (4–5 h) |
| R7 Campus + Reports + Governance + Accreditation | `reskin2/r7` | `/campus` 16 tabs → Overview / Spaces / Services / Resources / Setup with grouped sub-sections; overview = rooms-today bars, transport On-time badges, hostel beds bar, maintenance tickets; `/reports` = ReportCards + chart dialog + CSV; Governance + Accreditation → workspace-detail pattern under Reports; student Library | `adm-campus`, `adm-reports*`, `*-ws-*` (pattern), `stu-library` | R0 | L (7–8 h) |
| R8 Integrate | `reskin2/integrate` | merge R1 → R7 in order, resolve `reskin2-seed.mjs` / suite conflicts, full CI, side-by-side PDF for the user, then **ask the user before erp_merge** | all | all | S–M (2–3 h) |

R1–R7 run in parallel (disjoint feature folders). Only R0 edits `packages/ui`; requests come back as `NEEDS-R0` and are
added on `reskin2/foundation` (slices rebase).

## Progress accounting ("PROGRESS n%" status lines)
R0 = 0–12 %, R1–R7 = 12–90 % (≈ 11 points each, each worker reports its share), R8 = 90–100 %.

## Rules
`worker-common.md` (CI-only, push often, branch naming, keep all functionality and API calls, update e2e selectors, never
touch erp_merge) and `sutraerp/docs/reskin2-contracts.md` (component APIs, page anatomy: ≤5 tabs per area, one primary
action per row, no raw ids / codes, number formatting, table vs list).

## Risks / decisions
- **Data volume:** the old CI seed was sparse. `e2e/lib/reskin2-seed.mjs` seeds 16 students, a CS101 delivery with all
  of them seated, 10 tuition charges with postings and 12 active staff (Meera linked to the faculty login). Slices add
  their area (exam sessions, rooms / tickets, leave cases, payments, results …). Never invent figures in the UI to fill a
  prototype tile.
- **Prototype figures with no backend** (7-day KPI series, syllabus progress, transport on-time): show the tile without the
  sparkline / bar or an honest empty state and list it in the slice report.
- **Customizable Home (R1)** previously lived on `perf/phase2-4-final` (squash 2b397862, branch deleted on the user's OK;
  the sha may still be fetchable). Re-implement from the prototype if it is gone. A per-user preferences API is an API
  change → ask the user first; localStorage is the fallback.
- **First R0 shots (run 38690023551) noted:** `GET /finance/institutions/{id}/ledger-accounts` answers 409 when the chart
  was inserted by SQL (seed) — R5 should check the lazy chart provisioning against the seed; `GET /people/me/leave-types`
  404 "engagement not found" for Meera (staff record without an engagement) — R6 seeds an engagement.
- **Merge to erp_merge** only after the user approves the side-by-side review.
