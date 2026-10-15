# SutraERP feature gaps (Part 1 · Stage 1)

Consolidated from [`domains/`](domains/) at `f6ffff7`. Three kinds of gap per domain: (1) features that are **missing**, **deferred**
or **mock-only** in the inventory, (2) **API with no screen** — endpoints the web client never calls, which become dead ends
(or deadlocks, for decision endpoints) once DEF-X-001 is fixed, (3) **doc-claimed but missing** — documentation that says a
feature is done when it is not. `partial` rows are not repeated here; see the CSVs and [`DEFECT_REGISTER.md`](DEFECT_REGISTER.md).

| Domain | missing | deferred | mock-only | partial |
|---|---:|---:|---:|---:|
| A-platform | 16 | 1 | 0 | 9 |
| B-institution | 0 | 1 | 0 | 5 |
| C-student | 0 | 0 | 2 | 18 |
| D-academic | 1 | 0 | 0 | 16 |
| E-people | 7 | 0 | 0 | 17 |
| F-finance | 2 | 2 | 1 | 4 |
| G-campus | 9 | 4 | 1 | 13 |
| H-services | 6 | 6 | 1 | 7 |
| I-exams | 0 | 1 | 0 | 10 |
| J-resources | 1 | 8 | 0 | 1 |

## A-platform

| Id | Status | Feature | What is missing |
|---|---|---|---|
| A-001 | missing | L2-A01 Tenant provisioning (tenant aggregate, provisioning authority + events) | No tenant aggregate/table/API; tenant id comes only from the IdP claim or dev persona (modules/platform/src/auth/dev.rs, auth/keycloak.rs). Provisioning amendment 2026-08-26 not implemented |
| A-004 | missing | L2-A01 Platform governance decision model + architecture compliance process | No governance decision or compliance-process implementation (Sections 8.8, 10.16) |
| A-005 | missing | L2-A02 Person creation + canonical person-identity establishment and activation | No Person/Identity/User-Account aggregate; principals are 3 seeded dev personas or Keycloak identities; reconciliation amendment 2026-09-23 not implemented |
| A-006 | missing | L2-A02 Person deactivation / reactivation / archive / anonymization / permanent deletion | None implemented (no person aggregate) |
| A-007 | missing | L2-A02 Duplicate detection / review / merge / split / verification | None implemented |
| A-016 | deferred | L2-A03 Password change + password reset | Not in the app: delegated to Keycloak (keycloak mode); dev mode has a shared constant password (SEC-9D-014). No in-app link to the IdP account console found |
| A-020 | missing | L2-A03 Administrative session revocation (admin revokes another user's sessions) | Session revoke is bound to the caller's own subject (http_security.rs:264); no admin endpoint |
| A-023 | missing | L2-A04 Role management + role/permission assignment + active role resolution | Roles are a hard-coded enum (administrator/faculty/student) with a static grant table; no role or permission assignment API/UI, no reserved-role governance |
| A-025 | missing | L2-A04 Advanced authorization: delegation, temporary access, separation of duties, break-glass | None implemented |
| A-026 | missing | L2-A04 Authorization policy management (policy categories, evaluation order, versioning) | Only the compiled role table + step-up list; no policy store or ABAC evaluation |
| A-028 | missing | L2-A05 Workflow definition / version governance + migration | BPMN generated per key; no definition versioning, governance authority or instance migration |
| A-029 | missing | L2-A05 Human task management (assignment, claim, delegation, escalation, SLA, work queues) | No approval task inbox; Flowable decide tasks are completed only by decision sync from domain events; to-dos are not workflow tasks |
| A-030 | missing | L2-A05 Workflow recovery (suspension, cancellation, retry, compensation) + recovery authority | None implemented (amendment 2026-09-23) |
| A-036 | missing | L2-A07 Audit investigation, timeline generation, evidence packaging | No audit query/search/timeline/export API or UI |
| A-037 | missing | L2-A07 Retention governance, evidence archival and restoration | No audit retention/archive/restore (retention sweepers exist only for sessions, notifications, notices, to-dos) |
| A-039 | missing | L2-A09 Master data (MD-01..MD-18: datasets, reference data, review/approve/publish, versions, import/export, localization) | No master-data service; reference values are enums in each module |
| A-040 | missing | L2-A10 Operational readiness governance & certification (assessment, evaluation, evidence, findings, certificates, monitoring) | Only Domain B institution activation readiness exists (see B-007); none of the 18 A10 workflows |

**API with no screen:** No admin API for another user's sessions; no human-task inbox (Flowable decide tasks are completed only by decision sync). Search API exists (`/platform/search`) but has no UI (see H).

**Doc-claimed but missing:** `docs/batches.md:9` Batch 1 '✅ complete' — true for batch scope only; 16 of 45 L2-A rows missing. `docs/batches.md:26/28` WebAuthn: dev-mode only, still 501 in Keycloak (production) mode.

## B-institution

| Id | Status | Feature | What is missing |
|---|---|---|---|
| B-024 | deferred | L2-B03 Physical campus attribute workflows (defined by L2-G10) | Owned by Domain G (campus infrastructure), out of Domain B scope |

**API with no screen:** Governance configs, org restructures, type migration, org-unit PATCH/history, campus PATCH/revisions, `PUT …/timezone`, profile/policy revisions, position PATCH/changes/revisions, calendar resolution, school-model PATCH/revise, structure revise, activation-requests list, every `…/decision` except activation and simulation (DEF-B-006); curriculum version editing (DEF-B-005).

**Doc-claimed but missing:** `README.md:240,262,279`, `shared/approval.rs:15-18`: AwaitDecision 'in later batches, Flowable' — still auto-approved after 9C. `docs/batches.md:43` Docker click-through: no CI evidence.

## C-student

| Id | Status | Feature | What is missing |
|---|---|---|---|
| C-037 | mock-only | Academic Bank of Credits synchronisation (APW-12) | NoopAbcRegistry always returns NotConfigured; no real ABC integration |
| C-049 | mock-only | External scholarship ecosystem sync (NSP etc.) | NoopExternalScholarships returns NotConfigured |

**API with no screen:** `/duplicate-cases*`, `/students/{id}/duplicate-candidates` (DEF-C-004); `/credit-transfers*`, `/programme-migrations*`, `/academic-breaks/{id}/{start,decision,withdraw}`, `/conferred-qualifications/{id}/{corrections,revoke}`, `/students/{id}/financial-responsibilities`, guardian consent withdraw / communication revoke, `/transfers/{id}/{decision,withdraw}`, `/identity-corrections/{id}/{decision,withdraw}`, `/registrations/{id}/decision`, `/progression-decisions/{id}/{decision,revisions}` (DEF-C-007).

**Doc-claimed but missing:** `docs/batch3-contracts.md:710` duplicate detection 'works on name + DOB + contact' — only a post-creation read endpoint (DEF-C-003). `docs/batches.md:61-65` 3A–3C ✅ while approvals are automatic and several workflows lack UI.

## D-academic

| Id | Status | Feature | What is missing |
|---|---|---|---|
| D-044 | missing | Flexible-learning analytics & compliance reporting (W75–W82) | No dashboards/analytics/compliance reports for D06 |

**API with no screen:** Attendance corrections request/decide (DEF-D-003); coursework submissions, marking, moderation and internal-assessment corrections (DEF-D-007); offering windows, restrictions, quotas, revisions, merge/split, reorganisations, templates, simulations, section activate/cancel, enrolment transfer, waitlist accept/remove, faculty assignment changes and policy decisions, timetable slot add/remove, syllabus coverage writes, monitoring exceptions/reopen (DEF-D-008); no decision endpoint at all for enrolment overrides (DEF-D-005).

**Doc-claimed but missing:** `docs/batches.md:78-82` 4B '… attendance correction', 'internal assessment + coursework' ✅ — corrections and marking have no UI (DEF-D-003, D-007). `teaching/attendance.tsx:105` UI copy promises a correction-request flow that does not exist. L2-D04 §23 'Submitted / optional approval' not implemented (DEF-D-004).

## E-people

| Id | Status | Feature | What is missing |
|---|---|---|---|
| E-005 | missing | E01 WF-010/011 Deputation initiate / complete | no deputation aggregate or endpoint (7D deferral list, docs/batches.md:105) |
| E-015 | missing | E03 WF-006 Faculty availability management | no availability model (7D deferral) |
| E-021 | missing | E04 HR-WF-003 Generic HR case / grievance / disciplinary | HR case aggregate (L2-E04 §6.4.1) not implemented |
| E-022 | missing | E04 HR-WF-004 Promotion | no promotion workflow (only generic engagement modify) |
| E-025 | missing | E04 7.10 HR compliance workflow | compliance case aggregate not implemented |
| E-055 | missing | Salary disbursement (bank transfer file, clearing 2410 salaries payable) | no payment step for net pay; salaries payable never cleared |
| E-056 | missing | Statutory compliance outputs (PF / ESI / PT / TDS challans, Form 16, returns) | no statutory remittance or filing outputs |

**API with no screen:** About 66 endpoints (DEF-E-006): separation decision/clearance/complete/list, probation reviews, faculty profile/eligibility/suspend/reactivate, certificate decision/issue, onboarding decision/cancel, engagement modify/transfer, position-assignment change, staff merge/corrections, qualification/document verification, leave adjustment/encashment/case decisions, payroll adjustments, payslip reissue/withdraw, statutory-rule maintenance, config supersede, salary-assignment end; off-cycle payroll runs unreachable (`payroll/api.ts:18`).

**Doc-claimed but missing:** `docs/batches.md:112-114` 7A–7C ✅ for 'approval' / 'request → decision' / 'calculate → approve → finalise' — automatic in every deployed configuration (DEF-E-001). Salary disbursement and statutory remittance undocumented (DEF-E-005, E-007).

## F-finance

| Id | Status | Feature | What is missing |
|---|---|---|---|
| F-010 | mock-only | F02 Online payment: order → checkout → confirm / signed webhook → completion | real gateway adapter (only Mock and Disabled exist) |
| F-026 | missing | F05 Accounting batches (posting batch approval / retry) | ACCOUNTING_BATCH_APPROVAL key exists but no posting-batch aggregate / endpoint |
| F-027 | deferred | F06 Budget and cost-centre management (proposal, virement, allocation, control) | F-033…F-038 deferred (docs/batch5-contracts.md:39); not delivered in Batch 9 |
| F-035 | missing | F10 Fund transfers between bank accounts | FUND_TRANSFER_APPROVAL key exists, no fund-transfer aggregate / endpoint |
| F-037 | deferred | F12 Asset financial accounting (capitalisation, depreciation, disposal) | F-067…F-072 deferred (docs/batches.md:122, 205) |

**API with no screen:** Bulk collection validate/execute, receivable write-off decision, dunning runs/cases, payment cancel/recover/chargeback, unidentified receipts investigate/refund, bank-account decision/suspend/resume/close/settlements, bank-transaction return, aid recover, dispute withdraw, fee-structure decision (DEF-F-008).

**Doc-claimed but missing:** `docs/batches.md:13` Batch 5 'planned' although 5A–5C are on `erp_merge` (corrected in this Stage 1 doc commit). `docs/batch5-contracts.md:39` budgets F-033…038 'Batch 9 (or a later 5D)' — not delivered and not listed as a remaining deferral. `docs/security/findings.md` SEC-9D-003 'fixed' while mock stays selectable in production (DEF-F-002; caveat added in this commit). UI 'Write-off decided' (`payments/index.tsx:390`).

## G-campus

| Id | Status | Feature | What is missing |
|---|---|---|---|
| G-006 | missing | G10 Compliance registration / renewal / inspection of infrastructure | L2-G10 Compliance Registration, Compliance Renewal, Inspection workflows absent |
| G-007 | missing | G10 Renovation, sustainability recording, security classification, disaster recovery, criticality | L2-G10 workflows (17A.106-110, Renovation, Sustainability, Security Classification, Disaster Recovery) absent; docs/batches.md lists G10 depth as deferred |
| G-016 | missing | G01 Emergency maintenance workflow | L2-G01 Emergency Workflow absent (no priority/emergency path) |
| G-017 | missing | G01 Vendor assignment & AMC workflows | L2-G01 Vendor Assignment + AMC absent (resources J has suppliers but no link) |
| G-018 | missing | G01 Utility monitoring & consumption workflows | L2-G01 Utility Monitoring + Consumption absent |
| G-019 | missing | G01 Facility inspection workflow | L2-G01 Inspection Workflow absent (only hostel room inspection exists) |
| G-024 | missing | G02 Safety, incident, inspection, compliance, consumables, coordination workflows | L2-G02 Safety/Incident/Inspection/Compliance/Consumable/Coordination absent |
| G-035 | missing | G03 Check-in, visitor and discipline workflows | L2-G03 Check-In, Visitor, Discipline absent |
| G-051 | missing | G05 Acquisition, reading room, stock verification | L2-G05 Acquisition/Reading Room/Verification absent |
| G-052 | deferred | G06 Institutional health services & medical | All L2-G06 workflows |
| G-053 | deferred | G07 Security operations (gate, visitor, vehicle, material pass, patrol, incident, lost & found) | All L2-G07 workflows |
| G-054 | deferred | G08 Housekeeping operations & service governance | All L2-G08 workflows |
| G-055 | deferred | G09 Teacher Guardian Scheme | All L2-G09 workflows |
| G-062 | mock-only | Campus workspaces W-G00..W-G10 (catalog drawer) | Workflow drawer advances stages in local state only |

**API with no screen:** 57 campus routes with no web caller (DEF-G-007): all student/staff self-service `/me/hostel*`, `/me/transport*`, `/me/tickets*`, `/me/reservations`; ticket merge, lab batch allocations, practical postpone, maintenance cycle defer/complete, capabilities/availability PUT, transport complaints, catalogue-record PATCH, driver-assignment end (DEF-G-010); decisions for maintenance plans, room transfers, reservations, decommissioning (DEF-G-006).

**Doc-claimed but missing:** `README.md:43` 'scaffolds' (corrected). `docs/batches.md:127` 8A 'service desk … requester verification', room reservations — API-only for requesters (DEF-G-007). Work-order approval key + BPMN unused (DEF-G-008).

## H-services

| Id | Status | Feature | What is missing |
|---|---|---|---|
| H-001 | deferred | H01 APAAR & DigiLocker account linking | RequestAPAARLink/DigiLocker commands, consent aggregate |
| H-002 | deferred | H01 Credential publication, retry, revocation, consent, republishing | All publication/retirement/republishing amendments |
| H-007 | missing | H02 Template management & approval, scheduling, escalation | L2-H02 template lifecycle + Notification_Template_Approval amendment; scheduling lifecycle (19.4) |
| H-010 | missing | H03 Saved searches, search preferences, result navigation, search governance | H03-W06..W08 + Search_Governance amendment |
| H-011 | deferred | H04 Placement lifecycle (employers, drives, screening, offers, internships) | All L2-H04 workflows 19.1-19.17 |
| H-012 | deferred | H05 Research (proposals, projects, grants, ethics, publications, patents, consultancy) | All L2-H05 workflows |
| H-013 | deferred | H06 Alumni engagement (membership, mentorship, chapters, donations, verification) | All L2-H06 engagement workflows; C08 transition exists upstream |
| H-017 | missing | H07 Clone/archive/restore/unpublish, templates, dashboard workspaces, personal layouts, sharing | L2-H07 51.4-51.6, 51.8, 51.10-51.13, 51.18-51.21 + governance lifecycle amendment |
| H-022 | missing | H08-W06 Compliance evidence mapping | No evidence mapping between reporting values and evidence |
| H-030 | missing | H09 Compliance requirement expiry reconciliation & framework review normalisation (amendments) | L2-H09 2026-08-27 / 2026-09-01 amendments |
| H-031 | deferred | H10 External education integration (providers, connectors, enrolment, credit transfer, certificate verification) | All L2-H10 workflows |
| H-032 | missing | H00 Services & intelligence governance review | No H00 review aggregate (G00 exists only for campus) |
| H-034 | mock-only | Services workspaces W-H01..W-H10 (catalog drawer) | Workflow drawer advances stages in local state only |

**API with no screen:** `/services/me/dashboards` (DEF-H-002); platform search API (DEF-H-004).

**Doc-claimed but missing:** `README.md:43` 'scaffolds' (corrected). 'Submitted to the regulator' is a typed reference (DEF-H-003). `modules/services/src/lib.rs:4-6` 'read-only projections' outdated (H09 writes aggregates).

## I-exams

| Id | Status | Feature | What is missing |
|---|---|---|---|
| I-011 | deferred | Practical & viva examination conduct (incidents, re-conduct, batch/resource conflicts) (L2-I06) | I-044…I-054, I-056, I-058 deferred to 6D (docs/batch6-contracts.md §1) |

**API with no screen:** 78 of 182 exam route paths have no web caller: conduct, conduct records, papers conducted, invigilation, seating publish/revise, exam exceptions (DEF-I-007); policy/session/schedule/registration/override/moderation/result-decision/closure decisions, session merge, registration amend/arrangements/scope-sync, batch reopen, supplementary authorise, grade-card reissue/withdraw, revaluation correct (DEF-I-016).

**Doc-claimed but missing:** `docs/batch6-contracts.md` §1 6C 'pending malpractice' withheld condition not implemented (DEF-I-004); §1 6B 'moderation under policy' — constants (DEF-I-012). `shared/grading.rs:6` 'until 6A's policy makes them configurable' — never made configurable. `README.md:43` 'scaffolds' and no Batch 6 section (corrected).

## J-resources

| Id | Status | Feature | What is missing |
|---|---|---|---|
| J-001 | missing | Procurement planning: annual plan, demand consolidation, procurement method/committee rules (L2-J01 planning) | no procurement plan aggregate; committee procurement (GD-J01-03) and institution-specific rules (GD-J01-04) absent |
| J-005 | deferred | Supplier qualification, risk, blacklisting, closure (L2-J02) | J-022…029 deferred to 8E (docs/batch8-contracts.md §1) |
| J-007 | deferred | Emergency purchase requisition (L2-J03) | emergency PR deferred to 8E |
| J-010 | deferred | Receipt correction, discrepancy handling (L2-J05) | deferred to 8E |
| J-013 | deferred | Expiry / controlled stock, valuation (L2-J06) | deferred to 8E |
| J-018 | deferred | Stock transfers, distribution, custody (L2-J07) | deferred to 8E |
| J-021 | deferred | Asset location transfer, missing asset, repair work order, archival (L2-J08) | deferred to 8E; depreciation/capitalisation deferred (F-067…) |
| J-022 | deferred | Physical verification, audit counts, variance and reconciliation (L2-J09) | entire J09 (blind counts, frozen scope, recounts, variance) deferred to 8E |
| J-023 | deferred | Disposal, scrap and write-off (L2-J10) | entire J10 deferred to 8E; only asset retirement exists |

**API with no screen:** 18 of 70 route paths: requisition amend/withdraw, PO amend, goods-receipt inspect, sourcing-event decision/cancel (award decision), supplier merge, stock-reservation fulfil, stock-issue cancel, store locations, stock movements, detail GETs (DEF-J-008). Faculty has no procurement self-service.

**Doc-claimed but missing:** `docs/batch8-contracts.md` §1 8D decisions described as 'step-up governed approvals' — auto-approved (DEF-J-001). J09/J10 deferred to an '8E' not scheduled in `docs/batches.md`. `README.md:43` 'scaffolds' (corrected).

## Cross-cutting

- `docs/batches.md:201` and `docs/batch9-contracts.md:263-276`: Flowable 'proved and switched on per environment' — switching it on does not stop auto-approval, because module `ApprovalMode` is hard-coded and the adapter defaults `autoApprove=true` (DEF-X-001; caveat added to `docs/batches.md` in this commit).
- No approvals inbox / human-task UI exists anywhere (L2-A05, row A-029); it is the prerequisite for DEF-X-001 across every domain.
- Customer validation is pending for every feature.
