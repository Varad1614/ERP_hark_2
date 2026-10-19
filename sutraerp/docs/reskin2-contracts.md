# Re-skin v2 contracts (R0 foundation)

Status: R0 foundation on branch `reskin2/foundation` (off `erp_merge` 4753de5f). Slices R1–R7 branch
from it as `reskin2/r1` … `reskin2/r7`; R8 integrates. Nothing lands on `erp_merge` without the user's OK.

**The only design source is the prototype** `SutraERP-Prototype-Lite.final.html` (captured as
`/workspace/work/ui-audit/proto/*.png`, 1440×960, Plus Jakarta Sans installed). Tokens, radii,
shadows and fonts in `packages/ui/src/styles/tokens.css` already match it 1:1; what changes is
**page structure**. The audit that lists every drift is `/workspace/work/ui-audit/gap-report.md`.

Companion files (repo copies are canonical; mirrors under `/workspace/work/{brief,tools}/`): `docs/reskin2/plan.md`,
`docs/reskin2/slice-briefs.md` (route → prototype PNG per slice), `docs/reskin2/worker-common.md` (CI-only rules),
`docs/reskin2/rs2-ci.sh` (CI runs / jobs for a branch).

Do **not** copy these prototype artefacts: the "Prototype · mock data" tag, the "Simulate progress"
buttons, and the Reports cards' unsized arrow glyph.

---

## 1. Ownership

| Path | Owner |
|---|---|
| `packages/ui/**` (components, styles, formatters) | **R0 only.** Slices never edit the design system. A missing prop or widget goes back to R0 as a follow-up commit on `reskin2/foundation`; slices rebase. |
| `apps/web/app/features/<area>/**`, `apps/web/app/routes/<area>…` | the slice that owns the area (see `docs/reskin2/slice-briefs.md`) |
| `apps/web/app/routes/app-shell.tsx`, `routes/home.tsx`, `features/home/**`, shell overlays | R1 |
| `apps/web/e2e/lib/reskin2-seed.mjs` | each slice adds its own `seed<Area>()` function + one `domain()` line; no edits to other slices' functions |
| `apps/web/e2e/reskin2-shots.e2e.mjs`, `routes/dev/catalogue.tsx` | R0 (slices may add a catalogue section only via R0) |
| `apps/web/e2e/<suite>.e2e.mjs` | the slice whose screens the suite drives updates its selectors |

## 2. Page anatomy (every area page)

An area is **one overview page** in the prototype composition:

```
AreaHeader            h1 + ONE live summary line   |  [search] [≤5 tabs | More ▾] [primary action]
FilterChips rows      (lists only: Programme / Semester / Status …)
KpiTile ×3–4          (money, counts, rates — formatted, with a sparkline only for a real series)
1–2 rich sections     ListRow lists, cards (PersonCard / ClassCard), DataTable when it is truly tabular
ApprovalCard list     (anything awaiting the admin's decision)
```

### 2.1 Header rules
- `AreaHeader` replaces `PageHeader` + `AdminScreen` breadcrumb subtitles. The summary line is a
  **live sentence built from data** ("4,812 enrolled · 25 recent records shown", "286 faculty on
  roll · 12 leave requests to review"). Never institution name / code / time zone / "Active" badge.
- **≤ 5 header tabs per area.** `AreaHeader` enforces it: with 6+ tabs it shows 4 and folds the
  rest into a **More ▾** menu (the active overflow tab shows as "More · Label"). Prefer regrouping
  over overflow: an area should end with 2–5 meaningful tabs, deeper functions in drawers / dialogs.
- Tabs, search and the primary action are **right-aligned in the same tile** (never a strip under the title).
- Detail pages (Student 360, class detail, course detail) use `back={{ label, to }}` for the eyebrow
  back link and their own ≤5 tabs.

### 2.2 Rows and actions
- **One primary action per row**, as a pill (`ListRow.primary` / `RowActions.primary`). Every other
  action goes into the kebab (`menu`) or the record's `SidePanel` actions. No clusters of text
  buttons ("Details Suspend Book Capacity Decommission" is forbidden).
- Destructive menu items set `danger: true`; confirmations stay in a dialog.
- A row that opens details sets `onOpen` (chevron) or makes the name a `NameCell` button.

### 2.3 Table vs list vs cards
| Use | When |
|---|---|
| `ListRow` list (`.rc` / `.tk`) | work queues, to-dos, requests, resources, sessions, anything with a status + one action |
| `PersonCard` grid | people directories (staff, faculty) |
| `ClassCard` / `CourseCard` grid | classes, courses, programmes (progress bar, next session) |
| `ApprovalCard` list | items waiting for approve / reject |
| `DataTable` | genuinely tabular records compared column by column (student records, invoices, ledger, grade sheet, marks). Name column = `NameCell` (initials avatar); status = `StatusBadge`; amounts right-aligned with `formatINR`; last column at most `RowActions` |
| `WeekGrid` | any timetable (faculty, student, admin section timetable) |
| `AttendanceGrid` | marking attendance for a session |
| `DateList` | dated schedules (exam calendar, events) |
| `BarChart` / `Sparkline` | monthly series, KPI trends (real data only) |

### 2.4 Dialogs, panels, forms
- Multi-step or create flows use `StepDialog` (eyebrow "Area · Action", h2, round close X,
  horizontal `Stepper`, primary pill "Next →" / final label, ghost Cancel). Single-step dialogs
  are a `StepDialog` without `steps`.
- Record / request detail uses `SidePanel` (chip, eyebrow, h2, facts `dl`, stages stepper, actions).
- Forms use `FormGrid` + `TextField`, `FormSelect` (styled native select), `DatePicker`
  (shows **dd MMM yyyy**, value stays ISO), `TagInput` (never comma-separated free text),
  `Checkbox` / `CheckboxList` (never bare `<ul>` + native checkboxes), `Toggle`, `TextArea`,
  `FileField`. Inline forms inside a tile follow the prototype `.x-form` (one row, button at the end).
- Never ask the user for a raw id / uuid: pick from a select, a search, or a row.

### 2.5 Empty states
`EmptyState` (icon + one line, optional action) **inside** the tile it belongs to. Never stack 2–4
empty tiles on one page: collapse an empty area into one tile with one action.

## 3. Component API (`@sutra/ui`)

All exported from `packages/ui/src/index.ts`; CSS in `styles/reskin2.css` (ported from the prototype
class named in each comment). Existing components (`BentoTile`, `Button`, `Badge`, `DataTable`,
`Drawer`, `Modal`, `Tabs`, `StatTile`, `Steps`, `Toast`, `CommandPalette`, `Shell` …) keep their API;
old pages may keep using them until their slice re-skins them.

| Component | Props (abridged) | Prototype |
|---|---|---|
| `AreaHeader` | `title, summary?, back?{label,to}, tabs?: AreaTab[]{id,label,to?,count?}, activeTab?, onTabChange?, linkComponent?, search?{label,placeholder?,value,onChange}, actions?, tabsLabel?, data-*` | `.ph` + right tabs/search/`.go` |
| `splitTabs(tabs, max=5)` / `MAX_AREA_TABS` | tab overflow rule | — |
| `FilterChips` / `FilterChipRows` | `label?, options: {id,label,count?}[], value, onChange, search?` | `.fl` / `.fc` rows |
| `ListRow` | `title, meta?, avatar?(name) \| icon?+tone? \| check?{checked,onChange,label}, badge?{label,kind}, value?, primary?{label,onClick,icon?,ghost?,disabled?,data-*}, menu?: MenuItem[], onOpen?, selected?, done?, data-*` | `.rc` / `.tk` / `.x-rs` |
| `RowActions` | `primary?, menu?, label?` (table cell) | — |
| `MenuButton` / `MenuList` / `useMenu` | `MenuItem{id,label,onSelect?,danger?,disabled?,hint?,active?,render?,separated?,group?}` | `.pop.mn` |
| `Initials` / `NameCell` | `name, size sm\|md\|lg\|xl, tone? (Tone \| "accent")` / `name, sub?, onClick?` | `.x-av` / `.x-pav` |
| `PersonCard` | `name, role?, detail?, tags?{label,kind}[], wide?, tone?, onClick?, span?` | `.x-sc` / `.x-sc.w` |
| `ClassCard` | `code, title, meta?, icon, tone?, facts?{label,value}[], progress?{label,value 0–100}, badge?, onClick?, span? (6)` | `.x-cc` |
| `CourseCard` | ClassCard + `faculty?, attendance?, units?{done,total}` | stu-courses |
| `ReportCard` | `icon, tone?, title, description?, chart?, onClick?, span? (4)` | `.x-rpt` (sized chart, no giant arrow) |
| `CardGrid` | wrapper, 12-col dense grid | `.x-pgrid` |
| `ProgressBar` / `KeyValue` | `value 0–100, kind?` / `label, value` | `.x-bar` / `.x-row` |
| `ApprovalCard` / `ApprovalList` | `icon, tone?, title, meta?, onApprove?, onReject?, approveLabel?, rejectLabel?, busy?, decided?{label,kind}, onOpen?` | finance "Fee adjustments" |
| `KpiTile` | `icon, tone, label, value (formatted), sub?, trend? (real series), trendLabel? ("7 days"), progress?, span? (3), onClick?, loading?` | `.k` + `.dspw` |
| `StatCells` | `cells{label,value,sub?,kind?}[], columns 2\|3\|4` | `.x-mt` / `.x-cn` |
| `Balance` | `icon?, tone?, label, used, total, unit?, caption?` | faculty leave `.x-bal` |
| `BarChart` | `data{label,value,display?,soft?}[], height?, note?, label, onSelect?` | finance monthly collections |
| `DateList` | `days{key,day,month,items{label,meta?}[]}[]` | exam calendar `.x-day` |
| `WeekGrid` | `days: string[], slots: WeekSlot{id,day,start,end,title,meta?,tone?}[], todayIndex?, startHour? (9), endHour? (17), onSlotClick?` | `.x-tt` |
| `AttendanceGrid` | `students{id,name,short?}[], marks: Record<id, "present"\|"absent"\|"late">, onChange(id, mark), disabled?, showUnmarked?, hint?` + `nextMark`, `countMarks` | quick grid `.dqgg` |
| `StepDialog` / `Stepper` | `open, onClose, eyebrow?, title, steps?, step?, primary?{label?,onClick?,disabled?,busy?,type?,form?}, onBack?, cancelLabel?, note?, footer?` | `.md` + `.x-mh` + `.x-stp` |
| `SidePanel` | `open, onClose, title, eyebrow?, chip?, facts?{label,value}[], stages?, stage?, actions?` | `.dr` |
| `FormGrid` / `Field` | `columns 1\|2\|3` / `label, hint?, error?, hideLabel?, wide?` | `.x-g2` / `.x-lf` / `.x-lbl` |
| `TextField`, `TextArea`, `FormSelect{options,placeholder?}`, `DatePicker`, `TagInput{value,onChange,options?,allowCustom?,name?}`, `Checkbox{label,description?,note?}`, `CheckboxList`, `Toggle`, `FileField{label,hint?,fileName?,onFile}` | native controls underneath (`name`, `required`, e2e `fill` / `selectOption` keep working) | `.x-in` / `.x-ta` / `.x-ck` / `.x-file` |
| `EmptyState` | `icon?, title, children?, action?, compact?` | `.empty` |
| `StatusBadge` | `status (wire code), labels?, kind?` | `.bd` |

## 4. Formatting (no raw numbers, ids or codes)

| Value | Function | Example |
|---|---|---|
| Money | `formatINR(v)` | `140000` → `₹1,40,000`; `"160000.00"` → `₹1,60,000`; `"46250.50"` → `₹46,250.50`; `-2500` → `−₹2,500` |
| Money in KPIs / charts | `formatINR(v, { compact: true })` | `₹3.4 Cr`, `₹92 L` |
| Money in ledgers / receipts | `formatINR(v, { paise: "always" })` | `₹1,25,000.00` |
| Counts / quantities | `formatNumber(v)` | `"12.00"` → `12`, `4812` → `4,812` |
| Days | `formatDays(v)` | `"12.00"` → `12 days`, `1` → `1 day` |
| Percent | `formatPercent(v, { ratio? })` | `82%`, `72.5%` |
| Dates | `formatDate` / `formatDateShort` / `formatWeekdayDate` / `formatDateTime` / `formatTime` | `15 Jul 2026` / `15 Jul` / `Tue 16 Sep` / `15 Jul 2026, 10:30` (IST) / `10:30` |
| Wire codes | `humanize(code)` / `StatusBadge` | `not_submitted` → `Not submitted`, `approvalPending` → `Approval pending` |
| Status colour | `statusKind(code)` | `overdue` → attention, `pending` → review, `paid` → done |
| People | `initialsOf(name)`, `toneFor(key)` | `Dr. Meera Shah` → `MS` |

Rules: never print a uuid; show the human number (roll no., invoice no.) or the name instead.
Codes such as `MAIN-B1-F1` only as a muted secondary `KeyTag` next to the human name. Amounts
are never `toFixed` and never ungrouped. Domain wording beats `humanize` (pass `labels`).
`features/finance` `formatInr` (always two decimals) stays for ledger screens until R5 moves them
to `formatINR(v, { paise: "always" })`.

## 5. Behaviour that must not change
- Every existing function, API call, permission check, optimistic-version (`If-Match`) and
  idempotency behaviour stays. A re-skin moves controls; it does not remove them. Anything that
  leaves a page goes into the More menu, a kebab, a side panel, or a dialog.
- No API / OpenAPI changes in R1–R7 (the OpenAPI drift job must stay green).
- `data-*` hooks used by e2e (`data-action`, `data-form`, `data-tab-link`, `data-kpi`, …) are kept
  or the suite is updated in the same slice. New hooks: `data-area-head`, `data-area-summary`,
  `data-tab`, `data-tab-more`, `data-chip`, `data-row-open`, `data-row-menu`, `data-menu-item`,
  `data-approval` / `data-approve` / `data-reject`, `data-kpi-tile`, `data-slot`, `data-mark` /
  `data-student`, `data-dialog-primary`, `data-tags` / `data-tag-option`.
- e2e suites run at **1440×960** (the prototype viewport); 1280-wide suites move to 1440 in their slice.

## 6. Visual proof
- `/dev/catalogue?role=administrator|faculty|student&theme=dark&open=dialog|panel` renders every
  component with fixture data. Served by `react-router dev` and builds with `VITE_UI_CATALOGUE=1`
  (all CI e2e builds); production images render "Not found".
- CI matrix suite **`reskin2`** (`pnpm e2e:reskin2`, `apps/web/e2e/reskin2-shots.e2e.mjs`) seeds
  `e2e/lib/reskin2-seed.mjs` and captures every screen per role (Home, nav pills, every header tab
  and More entry, Student 360 + tabs, common screens, account menu, bell, palette) plus the
  catalogue → artifact **`e2e-shots-reskin2`** with `manifest.json` (file, role, path, title),
  `seed-report.txt`, `problems.txt`. File names are stable: `<adm|fac|stu>-<route-slug>.png`.
- The integrator pairs each app PNG with its prototype PNG (mapping in the slice briefs) using
  `/workspace/work/ui-audit/pair.py`.

## 7. Definition of done for a slice
1. Every route in the slice brief matches its prototype PNG composition (header, tab count, KPI
   row, lists/cards, dialogs), with real data and the deviations in the brief fixed.
2. ≤ 5 header tabs per area; one primary action per row; no raw ids / codes / unformatted numbers.
3. All functionality reachable; existing e2e suites green with updated selectors; `reskin2` suite green.
4. CI fully green on `reskin2/rN`; a side-by-side sheet (app vs prototype) attached to the report.
