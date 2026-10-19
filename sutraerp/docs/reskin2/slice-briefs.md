# Re-skin v2 — ready-to-paste slice briefs (R1–R7, R8)

**Common header (paste above each brief):**
> SutraERP RE-SKIN v2, slice RN. Redo the slice's screens to match the prototype
> `/workspace/projects/63e5e795-01c5-4321-b2b9-1a04cf4103fd/files/uploads/SutraERP-Prototype-Lite.final.html` STRICTLY (the
> only UI/UX source; PNG captures in `/workspace/work/ui-audit/proto/`). Repo github.com/Varad1614/ERP_hark_2, app
> `sutraerp/`. Base `origin/reskin2/foundation` (R0, CI green — sha in rs2-status.txt), branch `reskin2/rN`. Read FIRST:
> `sutraerp/docs/reskin2-contracts.md` (LAW), `sutraerp/docs/reskin2/worker-common.md` (= `/workspace/work/tools/rs2-worker-common.md`:
> CI-ONLY — no local cargo / pnpm / typecheck / docker / chromium; push every file or two; poll `bash rs2-ci.sh reskin2/rN`
> with credentials [{"provider":"github"}], sleeping 4 min; never touch erp_merge; never edit packages/ui → `NEEDS-R0`),
> `sutraerp/docs/reskin2/plan.md`, and the gap rows for your routes in `/workspace/work/ui-audit/gap-report.md` §(b).
> Keep ALL functionality and API calls; update e2e selectors; add your demo rows in your own `seed<Area>()` in
> `apps/web/e2e/lib/reskin2-seed.mjs`. Prove it with the `e2e-shots-reskin2` artifact + side-by-side sheets (`pair.py`) in
> `/workspace/work/rs2-out/rN/`. Status lines to both rs2-status.txt files with "PROGRESS n%". If the same failure loops
> more than twice, stop and report.

**Shot names.** The `reskin2` suite names app PNGs `<adm|fac|stu>-<path with / → _>.png` (`/students/admissions` →
`adm-students_admissions.png`; Student 360 → `adm-student-360.png`, `adm-student-360_<tab>.png`; workspace detail →
`<r>-workspace-detail*.png`; overlays `<r>-menu.png`, `-bell.png`, `-palette.png`). `manifest.json` lists file, path, h1.

**R0 components** (`@sutra/ui`, live at `/dev/catalogue`, shots `*-catalogue*.png`): `AreaHeader` (≤5 tabs + More),
`FilterChips` / `FilterChipRows`, `ListRow`, `RowActions`, `MenuButton`, `Initials` / `NameCell`, `PersonCard`,
`ClassCard`, `CourseCard`, `ReportCard`, `CardGrid`, `ProgressBar`, `KeyValue`, `ApprovalCard` / `ApprovalList`,
`KpiTile`, `StatCells`, `Balance`, `BarChart`, `DateList`, `WeekGrid`, `AttendanceGrid` (+ `countMarks`, `nextMark`),
`StepDialog` / `Stepper`, `SidePanel`, `FormGrid`, `Field`, `TextField`, `TextArea`, `FormSelect`, `DatePicker`,
`TagInput`, `Checkbox` / `CheckboxList`, `Toggle`, `FileField`, `EmptyState` (`action`, `compact`), `StatusBadge`;
formatters `formatINR` (`compact`, `paise`), `formatNumber`, `formatDays`, `formatPercent`, `formatDate` / `Short` /
`WeekdayDate` / `DateTime` / `Time`, `humanize`, `statusKind`, `initialsOf`, `toneFor`, `plural`.

---

## R1 — Shell + Home (customizable) + menus / palette / bell / to-dos / notices / profile + avatar Personalization menu

Owns `routes/app-shell.tsx`, `routes/home.tsx`, `features/home/**`, `routes/{todos,notices,notifications,profile,security,mfa-enrol,workspaces,workspace,screen,sign-in}.tsx`, `components/*` (shell overlays), `lib/roles.ts` (nav / home config only). Suite `e2e/ui.e2e.mjs` (+ `passkey.e2e.mjs` selectors if touched).

| App route / state | App shot | Prototype PNG | Deviations to fix (gap-report rows) |
|---|---|---|---|
| `/sign-in` | `signin.png` | `signin.png` | Row 1 — already 3; copy only. |
| shell header + pills | every shot | `*-home.png` header, `*-mobile-*.png` | Row 2 — fine at 1440; no clipped faculty/student pills; mobile: pills → full-width row, bottom tab bar ≤760 px (`.dtab`). |
| avatar menu | `<r>-menu.png` | `adm-menu.png`, `adm-menu-personalization.png`, `fac-menu.png`, `stu-menu.png` | Row 3 — replace inline Display segmented with a collapsible **Personalization** group (Customize Home, Theme…, Layouts…, Focus mode, Print) + Install as app, Take the tour, Institution branding (admin), Switch role, Sign out. Keep Security / Two-step reachable (Profile or menu); keep `data-display` hooks or update `ui.e2e`. |
| theme drawer / layouts / focus / edit | — | `adm-pz-theme.png`, `adm-pz-layouts.png`, `adm-pz-focus.png`, `adm-pz-edit.png` | Absent today: theme drawer (light / dark / hc / auto, accent, density, radius), layout presets, focus mode (`html.dfocus`), lite (`html.lite`). |
| `/` Home ×3 | `<r>-home.png` | `adm-home.png`, `fac-home.png`, `stu-home.png` (+ `*-dark-home`, `*-hc-home`, `*-lite-home`, `*-tablet-home`, `*-mobile-home`) | Rows 6–8 — customize mode (top bar Add widget / Layouts / Theme / Undo / Reset / Publish as default / Done; drag + resize; per-tile colour), widget library (Early flags, Your week, Morning brief, Pinned workspaces, Sticky note, day ring), KPI → `KpiTile` with sparkline + "7 days" **only from real series**, Right-now empty copy as prototype. Persistence: localStorage per user + role unless the user approves a preferences API. Prior work: Phase 2+4 squash 2b397862 (`perf/phase2-4-final`, deleted) — reuse if fetchable, else re-implement. |
| home to-do drawer | — | `*-home-dlg-todo.png` | → `SidePanel` (dl + Mark done / Approve). |
| palette (Ctrl K) | `<r>-palette.png` | `*-palette.png` | Row 4 — actions ("Dark mode", "Approve …", "Students below 75%"), kbd footer, highlighted match. |
| bell | `<r>-bell.png` | `*-bell.png` | Row 5 — unread dots, grouped rows, Mark all read. |
| `/todos` | `<r>-todos.png` | `*-todos.png`, `*-todos-dlg-todo.png` | Row 11 — `.tk` rows (`ListRow check`), badges, drawer. |
| `/notices` | `<r>-notices.png` | `*-notices.png`, `*-notices-dlg-a2-not.png` | Row 12 — notice rows + drawer with Acknowledge. |
| `/notifications` | `<r>-notifications.png` | `*-bell.png` (list form) | Same row style as the bell. |
| `/profile` | `<r>-profile.png` | `*-profile.png` | Row 13 — big initials (`.x-me`), facts dl tiles. |
| `/account/security`, `/account/mfa` | `<r>-account_security.png` | none → `*-profile.png` tiles + `.rc` rows / sign-in card | Rows 14–15 — sessions / passkeys as `ListRow`s, not the `sb-pref-table` look. |
| `/workspaces`, `/workspaces/:key/:tab` | `<r>-workspaces.png`, `<r>-workspace-detail*.png` | `*-workspaces.png`, `*-ws-overview.png`, `*-ws-workflows.png`, `*-ws-records.png`, `*-ws-dlg-workflow.png` | Rows 9–10 — `AreaHeader` with icon chip + right-aligned tabs; workflow drawer = `SidePanel stages`. |
| `/:screen` stub | — | `.soon` | Row 16 — fine. |

Dark / hc shots must stay readable; update the `ui.e2e` checks.

## R2 — Students (Domain C) + Student 360

Owns `routes/admin/students.tsx`, `routes/admin/student-360.tsx`, `features/student/**`, `routes/student/{documents,registration}.tsx`. Suite `e2e/student.e2e.mjs` (→ 1440×960).

Tabs: Students area 7 (Students, Admissions, Registration, Progression, Completion & exits, Guardians, Scholarships) → **Directory · n / Admissions · n / Registration / Progression + More (Completion & exits, Guardians, Scholarships)** (or a tighter regroup ≤5). Student 360: 8 → **Overview / Academics / Attendance / Fees / Documents** (prototype); lifecycle (enrolment, registration, progression & credits, guardians, scholarships, completion & exit) via More + `SidePanel`s.

| App route | App shot | Prototype PNG | Deviations to fix |
|---|---|---|---|
| `/students` → directory | `adm-students.png`, `adm-students_directory.png` | `adm-students.png` | Row 17 — `AreaHeader` summary "N enrolled · M recent records shown", search "Search name or roll no.", primary **New admission** (ghost "Pin this view" only if real); `FilterChipRows` Programme / Semester / Fee status with counts; table Name (`NameCell`), Roll no., Programme, Semester, Attendance % badge, Fee status badge; row → Student 360. |
| `/students/admissions` | `adm-students_admissions.png` | `adm-students-dlg-ad-new.png` | Row 18 — applications with ONE primary (Offer / Review) + kebab (Reject, Withdraw); **New admission = 3-step `StepDialog`** (Applicant details → Programme & documents → Fee & confirm); offer dialog with eyebrow + close; documents → `TagInput`; dates → `DatePicker`. |
| `/students/registration` | `adm-students_registration.png` | none → `adm-ws-records.png` + drawer | Row 19 — rows + panel, one action per row. |
| `/students/progression` | `adm-students_progression.png` | none → ws records + drawer | Row 20. |
| `/students/exits` | `adm-students_exits.png` | none → `adm-todos-dlg-todo.png` (dl + stepper) | Row 21 — clearance as `SidePanel stages`. |
| `/students/guardians` | `adm-students_guardians.png` | none → students table with `NameCell` | Row 22. |
| `/students/scholarships` | `adm-students_scholarships.png` | none → finance `ApprovalCard`s | Row 23 — pending awards as approval cards. |
| `/students/record/:id/overview` | `adm-student-360.png`, `adm-student-360_overview.png` | `adm-students-detail-ad-stu.png` | Row 24 — `back` "Students", name + meta + fee badge, 5 tabs right-aligned, "At a glance" `StatCells` 2×2 + Contact dl; status history below; drop the far-right "All students" link. |
| `/students/record/:id/*` | `adm-student-360_<tab>.png` | `adm-students-detail-tab-academics/attendance/fees/documents.png` | Rows 25–31 — stat cells + table per tab; lifecycle commands → More / panels; no inline command clusters. |
| `/documents` (student) | `stu-documents.png` | `stu-documents.png` | Row 103 — inline New request form (`FormSelect` + purpose + Submit, `.x-form`) + Your requests table. |
| `/registration` (student) | `stu-registration.png` | none → `stu-documents` form + stepper dialog | Row 105 — `CheckboxList` with credits notes, totals, primary Submit; nav highlights Courses. |

Seed: a guardian or two, admission applications, a scholarship if the API allows.

## R3 — Academics + faculty / student teaching (attendance grid + timetable)

Owns `routes/admin/{academics,academic-delivery}.tsx`, the academics parts of `features/institution/**` (structure, programmes, curriculum, school model, calendar — the campus-setup tabs are R7), `features/academic/**`, `routes/faculty/{classes,attendance,my-students,resources}.tsx`, `routes/shared/timetable.tsx`, `routes/student/{courses,assignments}.tsx`. Suite `e2e/academic.e2e.mjs` (→ 1440).

Tabs: Academics (2 segmented views × 5 / 8 tabs) → `/academics` **Overview (programme cards + courses table) / Curriculum / Calendar / Delivery + More (structure, school model, …)**; `/academic-delivery` 8 → ≤5 (Offerings / Timetable / Attendance / Assessment + More: faculty assignment, monitoring, flexible learning, credit recognition) — or fold delivery under `/academics` (old URLs keep working).

| App route | App shot | Prototype PNG | Deviations to fix |
|---|---|---|---|
| `/academics/*` | `adm-academics*.png` | `adm-academics.png`, `adm-academics-dlg-a2-addc.png` | Rows 32–36 — programme `ClassCard`s (enrolled / intake bars), Courses table with search + **Add course** `StepDialog`; no key:name:level text inputs (`FormGrid` + `FormSelect` + `DatePicker`). |
| `/academic-delivery/*` | `adm-academic-delivery*.png` | none → `adm-academics.png` courses table / `fac-timetable.png` | Rows 37–44 — ONE primary + kebab (Suspend / Freeze / Complete / Hide → menu); section timetable as `WeekGrid`. |
| `/classes` | `fac-classes.png` | `fac-classes.png` → `fac-classes-CS201.png`, `-tab-materials.png`, `-tab-sessions.png` | Row 87 — `ClassCard`s (code chip, next session, syllabus progress only with data) → **new class detail** (Roster / Materials / Sessions, back eyebrow). |
| `/attendance` | `fac-attendance.png` | `fac-attendance.png`, `-dlg-qgsub.png`, `-dlg-fa-submit.png` | Row 89 — class pill switcher (`FilterChips`), **`AttendanceGrid`** (Quick grid / List toggle), "Today's count" `StatCells` 3-up, Submit tile, confirm `StepDialog`; marks map → the existing attendance payload / API calls. |
| `/my-students` | `fac-my-students.png` | `fac-students.png`, `fac-students-dlg-fa-stu.png` | Row 88 — search, `NameCell`, attendance % badge, last grade, `SidePanel`. |
| `/resources` | `fac-resources.png` | `fac-resources.png`, `-dlg-f2-up.png`, `-dlg-f2-del.png` | Row 92 — per-class grouped `ListRow`s (type chip, size / date, Visible-to-students badge, delete), Upload `StepDialog` with `FileField` + `Toggle`. |
| `/timetable` (faculty) | `fac-timetable.png` | `fac-timetable.png`, `fac-timetable-dlg-fa-slot.png` | Row 91 — 3 `KpiTile`s (Teaching slots / Scheduled hours / Consultation) + **`WeekGrid`** 09:00–17:00 with Today pill + slot `SidePanel`. |
| `/timetable` (student) | `stu-timetable.png` | `stu-timetable.png`, `stu-timetable-dlg-st-slot.png` | Row 98 — same. |
| `/courses` | `stu-courses.png` | `stu-courses.png`, `stu-courses-CS201.png` | Row 99 — `CourseCard`s → course detail; Registration as a header action. |
| `/assignments` | `stu-assignments.png` | `stu-assignments.png`, `-tab-submitted.png`, `-tab-graded.png` | Row 100 — header tabs Pending / Submitted / Graded + `ListRow`s + submit `StepDialog` with `FileField`. |

Seed: CS101 timetable slots Mon–Sat, sessions with attendance, resources, assignments where the API allows. Carry-over: duplicated NPTEL label in setup.

## R4 — Exams (Domain I)

Owns `routes/admin/examinations.tsx`, `features/exams/**`, `routes/faculty/assessments.tsx`, `routes/student/results.tsx`. Suite `e2e/exams.e2e.mjs`.

Tabs: 7 (Sessions & policy, Timetable & seating, Registrations & hall tickets, Marks & moderation, Malpractice, Results & publication, Revaluation) → **Overview / Schedule / Marks / Results + More (Registrations & hall tickets, Malpractice, Revaluation, Sessions & policy)**.

| App route | App shot | Prototype PNG | Deviations to fix |
|---|---|---|---|
| `/examinations` (new overview) + `/examinations/*` | `adm-examinations*.png` | `adm-examinations.png` | Rows 45–51 — 3 `KpiTile`s, "Mid-semester calendar" `DateList`, "Result verification" `ApprovalCard`s (Verify pill); live summary instead of institution breadcrumb + Active badge; `sb-btn--sm` clusters → one primary + kebab; native selects → `FormSelect`. |
| `/assessments` (faculty) | `fac-assessments.png` | `fac-assessments.png` | Row 90 — KPI ×3 + `.rc` rows (icon chip + status badge + chevron); gradebook inline mark inputs styled as `.x-mk` (84 px, centred, red when invalid). |
| `/results` (student) | `stu-results.png` | `stu-results.png`, `stu-results-tab-semester-1.png` | Row 101 — KPI ×3 + semester header tabs + grade table; My examinations / revaluation as secondary tiles without stacked empty states. |

Seed: an exam session with a schedule (dates for the `DateList`) and marks pending verification.

## R5 — Finance (Domain F) + student Fees

Owns `routes/admin/finance.tsx`, `features/finance/**`, `routes/student/fees.tsx`. Suite `e2e/finance.e2e.mjs` (→ 1440).

Tabs: 9 (Fees & charges, Adjustments, Financial aid, Collections, Refunds, Receivables, Ledger, Periods, Bank & treasury) → **Overview / Collections / Receivables / Ledger + More (Fees & charges, Adjustments, Financial aid, Refunds, Periods, Bank & treasury)**.

| App route | App shot | Prototype PNG | Deviations to fix |
|---|---|---|---|
| `/finance` (new overview) + `/finance/*` | `adm-finance*.png` | `adm-finance.png` | Rows 52–60 — 4 money `KpiTile`s (Collected / Target / Pending / Collection rate, `formatINR compact`), Monthly collections `BarChart` (real receipts by month + note), Fee adjustments `ApprovalCard`s, Invoices table with status `FilterChips` (All / Paid / Pending / Overdue); amounts via `formatINR` (ledgers `paise: "always"`); no two-up empty tiles. |
| `/fees` (student) | `stu-fees.png` | `stu-fees.png`, `stu-fees-dlg-fe-pay.png` | Row 102 — Status / Next due / Paid KPIs, invoice table with **Pay now** pill, Pay `StepDialog` (UPI / card / net banking `.x-pm` choice → existing mock-gateway checkout; receipt number `.x-rno`); no bullet lists. |

Seed: payments against some of the 10 seeded charges (receipts by month), adjustments awaiting approval. Check: the R0 shots logged `GET /finance/institutions/{id}/ledger-accounts → 409` with the SQL-seeded chart — align the seed with the lazy chart provisioning (or fix the seed).

## R6 — People (Domain E) + faculty Leave

Owns `routes/admin/people.tsx`, `routes/faculty/leave.tsx`, `features/people/**`. Suite `e2e/people.e2e.mjs`.

Tabs: admin 7 (Staff records, Recruitment & onboarding, Positions & reporting, Leave, Attendance, Workload, Payroll) → **Directory · n / Leave requests · n / Payroll + More (Recruitment & onboarding, Positions & reporting, Attendance, Workload)**; faculty My HR 5 (Leave, My record, Attendance, Workload, Payslips) → prototype **Leave page** as default with ≤5 tabs.

| App route | App shot | Prototype PNG | Deviations to fix |
|---|---|---|---|
| `/people` → staff | `adm-people.png`, `adm-people_staff.png` | `adm-people.png` | Rows 61–63 — department `FilterChips` + search + `PersonCard`s (wide cards for leaders), header tabs with counts; no Status select + DataTable; engagement drawer → `SidePanel`. |
| `/people/leave` | `adm-people_leave.png` | `adm-people-tab-leave-requests.png` | Row 64 — leave requests as `ApprovalCard`s / `ListRow`s with Approve + kebab. |
| `/people/{recruitment,positions,attendance,workload,payroll}` | `adm-people_<tab>.png` | none → `adm-people.png` tiles + ws records | Rows 62–67 — one primary per row; payroll amounts `formatINR`. |
| `/leave/*` (faculty) | `fac-leave*.png` | `fac-leave.png` | Rows 93–97 — 3 `Balance` tiles, inline Apply form (`FormSelect` type, `DatePicker` from / to, reason, Submit in one `.x-lf` row), History table; `formatDays` (no "12.00 days"); apply stays inline. |

Seed: the 12 staff exist (R0); add departments / engagements (Meera's missing engagement makes `GET /people/me/leave-types` 404), leave balances, 2–3 pending leave cases.

## R7 — Campus + Reports + Governance + Accreditation (G / H / J): Campus 16 tabs → ≤5

Owns `routes/admin/{campus-setup,reports}.tsx`, `features/campus/**`, `features/reports/**`, the campus-setup tabs of `features/institution/**` (institution, readiness, campuses, organisation, positions, policies), `routes/student/library.tsx`. Suite `e2e/campus.e2e.mjs`.

Campus (6 setup + 10 ops = 16) → **Overview / Spaces / Services / Resources / Setup** with grouped sub-sections inside each tab (sub-section chips or tiles, not more tabs):
- Overview — rooms today (occupancy bars), transport (On-time badges), hostel beds bar, maintenance tickets (Mark resolved): the prototype page.
- Spaces — Infrastructure & rooms, Laboratories, Hostel.
- Services — Service desk & maintenance, Transport, Library (admin).
- Resources — Procurement, Inventory, Assets.
- Setup — Institution, Activation readiness, Campuses, Organisation, Positions, Policies.
- Governance (G00) moves to Reports. Old `/campus/<tab>` URLs keep working (redirect / map to tab + sub-section).
Reports (Dashboards, Regulatory reporting, Accreditation) + Governance → **Overview (report cards) / Regulatory / Accreditation / Governance**.

| App route | App shot | Prototype PNG | Deviations to fix |
|---|---|---|---|
| `/campus` + setup tabs | `adm-campus*.png` | `adm-campus.png` | Rows 68–73 — profile dl + "Suspend Archive Edit Rename" cluster → one primary + kebab / `SidePanel`. |
| `/campus/{infrastructure,facilities,labs,hostel,transport,library,procurement,inventory,assets}` | `adm-campus_<tab>.png` | `adm-campus.png` (+ `adm-ws-records.png` pattern) | Rows 74–82 (score 0) — no clipped strip; 4–5 text actions → one primary + kebab; raw codes (MAIN-B1-F1, SUP-000001) only as muted `KeyTag` beside a human name; `formatINR` (no 65000.00); tree with indentation + icon chips, not dots; hostel / transport occupancy bars and On-time badges. |
| `/campus/governance` → `/reports/governance` | `adm-campus_governance.png` | `adm-ws-overview.png`, `adm-ws-workflows.png`, `adm-ws-records.png`, `adm-ws-dlg-workflow.png` | Row 83 — workspace-detail pattern (Overview KPIs + Workflows rows + Records table + workflow `SidePanel` with stages); Open / Start / Cancel clusters → one primary + kebab; no 3 stacked empty tiles. |
| `/reports/*` | `adm-reports*.png` | `adm-reports.png`, `adm-reports-dlg-a2-rpt.png` | Rows 84–86 — six `ReportCard`s with sized mini charts (no giant arrow) opening a chart dialog with CSV; dashboards / versions / assignments behind the cards; accreditation = workspace-detail pattern. |
| `/library` (student) | `stu-library.png` | `stu-library.png` | Row 104 — Borrowed list with Renew + Catalogue with search, Available / Issued badges, Reserve; no stacked empty tiles. |

Seed: buildings / rooms, service tickets, hostel beds, a transport route, library items + a loan, a governance case. Batch 8 gaps to fix: raw record-id inputs in the library member and hostel apply-on-behalf forms (→ person pickers); approval-state wire values (`not_requested` / `not_submitted` / `approvalPending`) → `StatusBadge` labels.

## R8 — Integrate

Branch `reskin2/integrate` off `reskin2/foundation` (with any NEEDS-R0 additions). Merge `reskin2/r1` … `r7` in order
R1, R2, R3, R4, R5, R6, R7; resolve conflicts in `e2e/lib/reskin2-seed.mjs` (keep every `seed<Area>()`), `routes.ts`,
the shared harness. Full CI green (8 e2e suites incl. `reskin2`). Side-by-side PDF: every `manifest.json` entry with a
prototype counterpart (tables above) → `pair.py` sheets → `/workspace/work/rs2-out/reskin2-side-by-side.pdf`. Re-score
every gap-report row (target: no 0 / 1). Then **ask the user before anything goes to erp_merge** (squash candidate
`reskin2/squash` = erp_merge + one commit, as in Batch 7).
