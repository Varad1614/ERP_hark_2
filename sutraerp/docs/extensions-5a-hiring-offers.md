# 5A — Offers, onboarding checklists, follow-ups, hiring analytics, public careers page

Port of HeyPuter/everything `src/hiring` at commit 06802a1 (MIT): `src/core/offers.js`,
`onboard.js`, `followups.js`, `analytics.js`, `pipeline.js`, `retention.js`, `reminders.js`
(offers / follow-ups / onboarding reminders), `jobpage.js` (`escapeHtml`, `descriptionHtml`,
`subdomainBase`) and `emails.js` `renderTemplate`. Credited in `THIRD_PARTY_NOTICES.md`.

It **upgrades** the existing recruitment → onboarding flow (`modules/people/src/hr/recruitment.rs`,
`onboarding.rs`); nothing is duplicated: the candidate's own statuses stay the source of truth, the
offer letter drives them, and the existing onboarding case (which creates the staff record at
joining) gains a checklist.

## Pure modules

| File | What |
|---|---|
| `modules/people/src/hr/offers.rs` | offer status machine (`draft → pending_approval → approved → sent → accepted / declined / expired / withdrawn`), `validate_offer` (Everything's rules + codes), `validate_ctc`, CTC breakup (`Ctc`: basic, HRA, DA, allowances, employer PF per month; `annual()` = 12 × monthly), `suggested_employer_pf` (12 % of basic + DA up to the ₹15,000 EPF wage ceiling), `format_inr` (Indian grouping, paise only when present), `amount_in_words` (lakh / crore), `letter_date` (`1 June 2026`), letter templating (`render_template`, `render_offer_letter`: a line whose placeholders are all empty is dropped), `DEFAULT_OFFER_TEMPLATE`, `letter_vars`, `letter_html`; onboarding templates (`normalize_templates`, `template_for_job`, `default_template` — Indian college checklist with owners hr / it / department / accounts / admin), `build_plan`, `task_due_date`, `offset_label`, `hire_start_date`, `due_onboarding_tasks`, `progress`, `group_tasks`; follow-ups (`validate_followup`, `followup_due`, `due_followups`, `sort_followups`). |
| `modules/people/src/hr/hiring_analytics.rs` | funnel mapping (`sutra_stage`: applied → `applied`, shortlisted → `screen`, interviewed → `interview`, offered → `offer`, offer_accepted → `hired`; rejected / offer_declined → archived `rejected`, withdrawn → `withdrawn`), `stage_keys_from`, `stage_visits`, `reached_stages`, `stage_conversion`, `time_to_hire`, `average_days_in_stage`, `source_breakdown`, `offer_outcomes`, `funnel_counts`, `days_in_stage`, `stale_candidates`, `archived_at`, `retention_cutoff`, `retention_due`, `compute_reminders` (stable keys `offer-…-soon|expired`, `followup-…`, `onboarding-…`). |
| `modules/people/src/hr/careers.rs` | `escape_html`, `description_html`, `slug_base`, `is_valid_slug`, `check_application` (honeypot `website`, size limits, phone 10–15 digits, http(s) link). |

**Golden vectors**: spec `tools/golden/specs/hiring-offers.mjs` → fixture
`modules/people/tests/golden/hiring_offers.json` (200 cases; each case exercises every ported
function), replayed by `modules/people/tests/golden_hiring_offers.rs`. Mapping in the replay:
Everything `signed` = SutraERP `accepted`; Everything `draft → sent` = SutraERP
`draft → pending_approval → approved → sent`; an unknown status behaves as terminal; the default
checklist template in the replay is Everything's (SutraERP ships an Indian one); labels say
"joining date" where Everything says "start date".

Indianisation kept out of the golden inputs (unit tests in `offers.rs`): ₹ formatting, amount in
words, employer PF, the letter template and dates.

## Tables — migration `20270104000100_people_hiring_offers.sql`

* `people.hiring_settings` + `offer_template`, `onboarding_templates` (jsonb), `stale_after_days`
  (14), `retention_months` (0 = keep), `careers_enabled`, `careers_slug` (unique across tenants),
  `careers_intro`, `careers_contact_email`.
* `people.requisition` + `careers_published`, `public_description`, `public_location`, `closes_on`.
* `people.candidate` + `talent_pool` (kept out of retention).
* `people.candidate_offer` — number `OFR-NNNNNN`, terms, monthly CTC columns (numeric 12,2),
  status, `letter_text` (snapshot at send / accept), `drafted_by`, `submitted_by`, `decided_by`
  (CHECK: ≠ drafter and submitter), `sent_at`, `accepted_by_name`, `resolved_at`, version. One
  offer in play per candidate (partial unique index).
* `people.onboarding_task` — checklist of an onboarding case (position, title, category, owner,
  `offset_days` from the case's joining date, done / done_at / done_by).
* `people.candidate_followup` — text, due date, optional `HH:MM`, done / completed_at.
* `people.hiring_reminder` — reminders recorded by the hourly job, primary key
  `(tenant_id, reminder_key)` so each nudge is recorded once.

RLS: `platform.apply_tenant_rls()`. Grants to `sutra_people`.

## Endpoints (tag `people-hr`)

| Method + path | Op id | Permission |
|---|---|---|
| `GET/PUT /people/institutions/{id}/offer-settings` | `people_recruitment_offer_settings_get/put` | read / staff.manage |
| `GET /people/institutions/{id}/offers?status=` | `people_recruitment_offer_institution_list` | read |
| `GET/POST /people/candidates/{id}/offers` | `people_recruitment_offer_list/create` (Idempotency-Key) | read / staff.manage |
| `GET/PUT /people/offers/{id}` | `people_recruitment_offer_get/update` (If-Match; draft only) | read / staff.manage |
| `POST /people/offers/{id}/submit` | `people_recruitment_offer_submit` (If-Match) | staff.manage |
| `POST /people/offers/{id}/decision` | `people_recruitment_offer_decide` (If-Match; SoD) | **people.approve** |
| `POST /people/offers/{id}/send` | `people_recruitment_offer_send` (If-Match) | staff.manage |
| `POST /people/offers/{id}/accept` | `people_recruitment_offer_accept` (If-Match) | staff.manage |
| `POST /people/offers/{id}/decline` · `/withdraw` | `people_recruitment_offer_decline/withdraw` (If-Match, reason) | staff.manage |
| `GET /people/offers/{id}/letter` | `people_recruitment_offer_letter` (text + HTML) | read |
| `GET/POST /people/onboarding-cases/{id}/checklist` | `people_onboarding_checklist_get/add` | read / staff.manage |
| `POST /people/onboarding-tasks/{id}/complete` | `people_onboarding_checklist_complete` | staff.manage |
| `GET /people/institutions/{id}/onboarding-due` | `people_onboarding_checklist_due` | read |
| `GET/POST /people/candidates/{id}/followups` | `people_recruitment_followup_list/create` | read / staff.manage |
| `POST /people/candidate-followups/{id}/complete` | `people_recruitment_followup_complete` | staff.manage |
| `GET /people/institutions/{id}/hiring-followups` | `people_recruitment_hiring_followups` | read |
| `GET /people/institutions/{id}/hiring-analytics?requisitionId=&sinceDays=` | `people_recruitment_hiring_analytics` | read |
| `GET/PUT /people/institutions/{id}/careers-page` | `people_recruitment_careers_page_get/put` | read / staff.manage |
| `PUT /people/requisitions/{id}/careers-listing` | `people_recruitment_careers_listing_put` | staff.manage |
| `GET /people/public/careers/{slug}` | `people_recruitment_careers_public` | **public** |
| `POST /people/public/careers/{slug}/requisitions/{id}/apply` | `people_recruitment_careers_apply` | **public** |

All routes are under the `hr` module (entitlements `people` default). The two public routes are in
`apps/api/tests/auth_hardening.rs` `PUBLIC` and justified in
`docs/readiness/AUTHENTICATION_HARDENING.md`. The decision endpoint is in
`docs/readiness/APPROVAL_CONTROL_MATRIX.md`.

## Rules

* **Draft** only for a shortlisted / interviewed / offered candidate; designation and grade
  default to the requisition's; employer PF defaults to `suggested_employer_pf`; 422 codes:
  `ctc_required`, `bonus_negative`, `joining_date_required`, `expiry_malformed`,
  `expiry_after_joining`, `negative_component`, `pf_above_statutory`.
* **Submit** → `pending_approval`; with `SUTRA_APPROVAL_MODE=auto` (dev / test only) it is approved
  at once by `system:auto-approval`.
* **Decision** (`people.approve`): approve → `approved`; reject (reason) → back to `draft`. The
  drafter and the submitter get 403 (`ensure_independent_decider`), and the DB CHECK refuses it too.
* **Send** (approved, requisition open, not lapsed) → `sent`; the letter text is snapshotted;
  candidate shortlisted / interviewed → `offered` (with designation, joining date).
* **Accept** (sent, not lapsed, an opening left) → `accepted`; candidate → `offer_accepted` with the
  offer's joining date / designation (grade only when it is a valid grade code); the letter is
  re-snapshotted with "Accepted by …".
* **Decline** (reason) → candidate `offer_declined`. **Withdraw** (reason) leaves the candidate's
  stage; a revised offer can then be drafted.
* The legacy direct `candidates/{id}/accept|decline` also close a sent offer letter.
* **Onboarding**: `POST /candidates/{id}/onboarding` (existing) now stamps the checklist from the
  institution's templates (requisition id in `jobIds` → department of the accepted offer →
  general → the shipped default). Due date = case joining date + offset; overdue = past and not
  done. `onboarding-due` = unfinished tasks of live cases due within 3 days or overdue.
* **Follow-ups**: date-only ones are due from the start of their day, timed ones at their time
  (institution timezone); overdue as Everything.
* **Attention view**: reminders due now (offers expiring within 3 days / lapsed, due follow-ups,
  due onboarding tasks of hires), candidates in one stage ≥ `stale_after_days`, archived
  candidates past `retention_months` (DPDP Act 2023 — listed for a person to confirm, never erased
  automatically; talent-pool candidates are kept), the last 20 recorded reminders.
* **Analytics**: funnel (active per stage), conversion per stage, time to hire (median days,
  scoped by when the hire happened), average days per stage, sources, offer outcomes (acceptance =
  accepted / (accepted + declined); expired / withdrawn / in play counted), per requisition.
* **`HiringReminderJob`** (`apps/api/src/app.rs`, hourly with the sweepers, name
  `people_hiring_reminders`): per institution with live candidates, in its tenant context — sent
  offers whose expiry day has passed become `expired` (`system:hiring-reminders`, history +
  audit); reminders are inserted into `people.hiring_reminder` once per key. Delivery to the
  notification inbox is not wired yet (follow-up).
* **Careers page**: a slug (3–40 `a-z0-9-`, unique across SutraERP) and an enabled flag publish
  `/careers/<slug>` in the web app. Listed: requisitions `open` + `careers_published` + not past
  `closes_on`. Safe fields only (title, designation, category, employment type, openings,
  location, closing date, escaped description). Apply creates an `applied` candidate with source
  "Careers page" (history + audit by `public:careers-page`). Spam safety: honeypot `website`
  (silently dropped, 202), 16 KB body cap (413), field limits (422), duplicate e-mail on the same
  requisition (202, nothing stored), 200 applications per requisition per day (429), plus the API's
  public per-client rate limit.

## Web

* Admin → People → Recruitment: **Offers & onboarding** tile (offers list with the next action,
  draft form with CTC breakup and live ₹ totals, letter preview, onboarding checklists with
  progress, follow-ups / attention) and **Hiring analytics** tile, next to the interview
  scorecards tile. `apps/web/app/features/people/hiring/{OffersTile,HiringAnalyticsTile}.tsx`,
  API in `offersApi.ts` (local types; schema.d.ts is regenerated by the main agent).
* Public route `/careers/:slug` (outside the app shell, like sign-in): openings and the apply
  form (with the hidden honeypot).
