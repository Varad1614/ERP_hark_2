# Approval control matrix (Part 1 · Stage 2 · S2-appr)

Every governed decision in SutraERP, who may raise it (maker), who may decide it (checker), and
whether separation of duties (SoD) and an audit trail are enforced. Branch `part1/s2-appr`.

## How approval works now (DEF-X-001)

* **One switch, human by default.** `SUTRA_APPROVAL_MODE` = `human` (default) | `auto`
  (`crates/kernel/src/approval.rs`, read in `apps/api/src/config_b9.rs`, handed to every module
  state by the composition root `apps/api/src/app.rs`). No module hard-codes auto-approval any more.
* **Production refuses every auto path** (`apps/api/src/config.rs`): `SUTRA_APPROVAL_MODE=auto`,
  `SUTRA_WORKFLOW=noop` (also the unset default) and `SUTRA_FLOWABLE_AUTO_APPROVE=true` all stop
  start-up with `ConfigError::Forbidden`. The Helm chart refuses to render the same values when
  `global.environment=production` (`templates/core/configmap.yaml`) and production values run
  Flowable with `autoApprove: false` (`values.yaml`, `values-pilot.yaml`). The kind CI install
  (development) runs bundled Flowable with `approvals.mode: auto` because its seed stages walk every
  governed setup step; compose defaults to human (`SUTRA_COMPOSE_APPROVAL_MODE`).
* **Flowable** starts every process with `autoApprove=false` unless the adapter is explicitly
  allowed (`SUTRA_FLOWABLE_AUTO_APPROVE=true`, dev/test only) **and** the caller asks for it.
* **Dev/test opt-in.** Suites that test domain flows rather than approval set
  `SUTRA_APPROVAL_MODE=auto` explicitly: `apps/api/tests/support/mod.rs`, e2e `lib/harness.mjs`,
  `student.e2e.mjs`, `passkey.e2e.mjs`, the pilot / data / observability / quality workflows,
  `deploy/perf/run.sh`, the kind values and the compose boot job (demo seed).

## Separation of duties (DEF-X-002)

`ensure_independent_decider` (kernel + each module's `shared::approval`) refuses with **403** a
decision by an unidentified or `system:` actor, or by any maker of the request: its creator /
`requested_by` column and whoever moved it into its pending status (from the module's
`status_history`). Five checker roles hold a domain's read + approve permissions and **none** of the
manage permissions that create requests (`crates/kernel/src/auth.rs`,
`approver_roles_hold_approve_without_manage`); each has a dev persona
(`modules/platform/src/auth/dev.rs`):

| Role | Dev persona | Approve permissions |
|---|---|---|
| `finance_approver` | finance.approver@sutra.dev | finance.approve, library.fine.waive.approve (S2-ops, DEF-G-002) |
| `exam_controller` | exam.controller@sutra.dev | exam.approve, exam.result.publish |
| `registrar` | registrar@sutra.dev | admission.decide |
| `payroll_approver` | payroll.approver@sutra.dev | people.payroll.approve |
| `procurement_approver` | procurement.approver@sutra.dev | resources.approve |

`administrator` still holds every permission, so an administrator may act as checker, but never on
a request they made (SoD applies to every caller).

## Governed decisions with SoD enforced (15)

| Decision (operation) | Endpoint | Maker permission / role | Checker permission / role | SoD | Audit | Test |
|---|---|---|---|---|---|---|
| Refund | `POST /finance/refunds/{id}/decision` | refund.manage / administrator | finance.approve / finance_approver (or another administrator) | y | y (`finance.refund.approve`/`reject` + status history + `decided_by/at/decision_reason`) | `db_p1_appr::refund_waits_for_an_independent_finance_approver`; `db_b5b::approvals_await_a_decision` |
| Receivable write-off | `POST /finance/receivables/{id}/write-off/decision` | receivable.manage / administrator | finance.approve / finance_approver | y | y | `db_b5b::approvals_await_a_decision` |
| Finance adjustment | `POST /finance/adjustments/{id}/decision` | adjustment.manage / administrator | finance.approve / finance_approver | y | y | covered by the shared SoD path (`kernel::approval` unit tests); no dedicated DB regression yet |
| Payroll run | `POST /people/payroll-runs/{id}/decision` | people.payroll.manage / administrator | people.payroll.approve / payroll_approver | y | y | `db_b7c` (maker ≠ approver contexts); shared SoD unit tests |
| Payroll adjustment | `POST /people/payroll-adjustments/{id}/decision` | people.payroll.manage / administrator | people.payroll.approve / payroll_approver | y | y | shared SoD unit tests |
| Final settlement | `POST /people/final-settlements/{id}/decision` | people.payroll.manage / administrator | people.payroll.approve / payroll_approver | y | y | shared SoD unit tests |
| Result batch approval | `POST /exams/result-batches/{id}/decision` | exam.result.manage / administrator | exam.result.publish / exam_controller | y | y (`exams.result_batch.approve` + status history) | `db_p1_appr::result_batch_waits_for_an_independent_exam_controller`; `db_b6c::await_decision_keeps_governed_steps_pending` |
| Result decision (correction / withholding / withdrawal) | `POST /exams/result-decisions/{id}/decision` | exam.result.manage / administrator | exam.result.publish / exam_controller | y | y | `db_b6c::await_decision_keeps_governed_steps_pending` |
| Result publication authorisation | `POST /exams/publications/{id}/decision` | exam.result.publish (planner / authorisation requester) | exam.result.publish / exam_controller (a different user) | y | y | `db_b6c::await_decision_keeps_governed_steps_pending` |
| Admission | `POST /student/admissions/{id}/decision` | admission.manage / administrator | admission.decide / registrar | y | y | `db_b3a::admission_approval_await_decision_and_finance_evidence` |
| Purchase requisition | `POST /resources/requisitions/{id}/decision` | resources.procurement.manage / administrator | resources.approve / procurement_approver | y | y | `db_b8d::governed_decisions_await_a_decision` |
| Purchase order | `POST /resources/purchase-orders/{id}/decision` | resources.procurement.manage / administrator | resources.approve / procurement_approver | y | y | `db_b8d::governed_decisions_await_a_decision` |
| Vendor invoice | `POST /resources/vendor-invoices/{id}/decision` | resources.procurement.manage / administrator | resources.approve / procurement_approver | y | y | shared SoD unit tests |
| Vendor payment (S2-ops, DEF-J-003) | `POST /resources/vendor-payments/{id}/decision` (process `resources.vendor_payment_approval`) | resources.procurement.manage / administrator (create → `pending_approval`) | resources.approve / procurement_approver **or** finance.approve / finance_approver (or another administrator); posting (`/post`) only from `approved` | y (maker = `approval_requested_by` + history) | y (`resources.vendor_payment.decide` + `.approve`/`.reject` + status history + `decided_by/at/decision_reason`; events `vendor_payment.submitted/approved/rejected`) | `db_p1_ops::vendor_payment_posts_only_after_an_independent_approval`; `db_b8d::procure_to_pay_end_to_end` (auto opt-in) |
| Library fine waiver (S2-ops, DEF-G-002) | `POST /campus/library-fines/{id}/decision` | campus.library.manage / administrator (`/waiver` → `waiver_requested`; no amount threshold — every waiver needs a decision) | library.fine.waive.approve / finance_approver (or another administrator); `campus.approve` alone no longer decides it | y (assessor + waiver requester from `campus.status_history`) | y (`campus.library_fine.request_waiver` / `.waive` / `.reject_waiver` + status history; `library_fine.waived` event) | `db_p1_ops::library_fine_waiver_is_decided_by_another_authorised_user`; `db_b8c::library_await_decision_policy_fine_waiver_and_clearance` |

### Value controls (S2-ops, DEF-J-005)

The ₹1,00,000 direct-purchase limit (`sourcing::DIRECT_SOURCING_LIMIT`,
`sourcing::ensure_within_direct_limit`) binds the **actual** value, not only the requester's
estimate: a direct purchase order's total (caller-supplied prices), the quotation and award of a
direct sourcing event, and any amendment that raises a direct order are refused (409) above it —
only the competitive route (RFQ → evaluation → award, `AWARD_APPROVAL`) orders more. Every PO
amendment returns the order to `draft`, so the changed order is issued again only through
`PURCHASE_ORDER_APPROVAL` decided by an independent procurement approver. Test:
`db_p1_ops::direct_purchase_limit_binds_the_ordered_value`.

Configuration (production refusals): `db_p1_appr::production_config_refuses_noop_workflow_and_auto_approval`,
`config::tests` (unit), Helm checks `production-refuses-*` in `.github/workflows/sutraerp-deploy.yml`.

## Other governed decisions (human by default, SoD not yet enforced)

These wait for a decision under the default `human` mode (DEF-X-001 applies to all of them) but
the decider is checked only for the permission, not for independence from the requester. The
maker column is a code scan of the slice's manage permissions; the checker is the permission the
decision handler requires. Extending `ensure_independent_decider` to these is the open follow-up.

| Operation | Endpoint | Maker permission (slice) | Checker permission (roles) | SoD | Audit |
|---|---|---|---|---|---|
| `assessment_moderation_decision` | `/academic/assessment-windows/{id}/moderation/{moderationId}/decision` | AssessmentManage | AssessmentManage, in-handler check (administrator) | n | y |
| `attendance_correction_decision` | `/academic/attendance-corrections/{id}/decision` | AttendanceManage | AttendanceManage, in-handler check (administrator) | n | y |
| `attendance_policy_decision` | `/academic/attendance-policies/{id}/decision` | same slice manage permission | AttendanceManage (administrator) | n | y |
| `certificate_verification_decision` | `/academic/certificate-verifications/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `credit_mapping_decision` | `/academic/credit-mapping-rules/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `credit_recognition_decision` | `/academic/credit-recognitions/{id}/decision` | FlexibleLearningManage | CreditRecognitionDecide (administrator) | n | y |
| `faculty_policy_decision` | `/academic/faculty-assignment-policies/{id}/decision` | FacultyAssignmentManage | FacultyAssignmentDecide (administrator) | n | y |
| `faculty_assignment_change_decision` | `/academic/faculty-assignments/{id}/changes/{changeId}/decision` | FacultyAssignmentManage | FacultyAssignmentDecide (administrator) | n | y |
| `faculty_assignment_decision` | `/academic/faculty-assignments/{id}/decision` | FacultyAssignmentManage | FacultyAssignmentDecide (administrator) | n | y |
| `flexible_enrolment_decision` | `/academic/flexible-enrolments/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `flexible_exception_decision` | `/academic/flexible-exceptions/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `flexible_policy_decision` | `/academic/flexible-learning-policies/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `internal_assessment_correction_decision` | `/academic/internal-assessment-results/{id}/corrections/{correctionId}/decision` | AssessmentManage | AssessmentManage, in-handler check (administrator) | n | y |
| `learning_opportunity_decision` | `/academic/learning-opportunities/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `learning_provider_decision` | `/academic/learning-providers/{id}/decision` | same slice manage permission | FlexibleLearningManage (administrator) | n | y |
| `monitoring_case_decision` | `/academic/monitoring-cases/{id}/decision` | same slice manage permission | MonitoringManage (administrator) | n | y |
| `offering_decision` | `/academic/offerings/{id}/decision` | OfferingManage | OfferingDecide (administrator) | n | y |
| `offering_reorganisation_decision` | `/academic/offerings/{id}/reorganisations/{reorganisationId}/decision` | OfferingManage | OfferingLifecycle (administrator) | n | y |
| `offering_revision_decision` | `/academic/offerings/{id}/revisions/{revisionId}/decision` | OfferingManage | AcademicRead (administrator) | n | y |
| `offering_simulation_decision` | `/academic/offerings/{id}/simulations/{simulationId}/decision` | OfferingManage | OfferingDecide (administrator) | n | y |
| `internal_assessment_decision` | `/academic/sections/{id}/internal-assessment/decision` | AssessmentManage | AssessmentManage, in-handler check (administrator) | n | y |
| `timetable_decision` | `/academic/timetables/{id}/decision` | TimetableManage | TimetablePublish (administrator) | n | y |
| `campus_governance_case_decide` | `/campus/governance-cases/{id}/decision` | GovernanceManage | CampusApprove (administrator) | n | y |
| `campus_governance_config_decide` | `/campus/governance-configs/{id}/decision` | GovernanceManage | CampusApprove (administrator) | n | y |
| `campus_hostel_application_decide` | `/campus/hostel-applications/{id}/decision` | HostelManage | CampusApprove (administrator) | n | y |
| `campus_hostel_policy_decide` | `/campus/hostel-policies/{id}/decision` | HostelManage | CampusApprove (administrator) | n | y |
| `campus_infra_node_decommission_decide` | `/campus/infrastructure-nodes/{id}/decommission-decision` | FacilityManage | CampusApprove (administrator) | n | y |
| `campus_library_clearance_decide` | `/campus/library-clearances/{id}/decision` | LibraryManage | CampusApprove (administrator) | n | y |
| `campus_library_policy_decide` | `/campus/library-policies/{id}/decision` | LibraryManage | CampusApprove (administrator) | n | y |
| `campus_maintenance_plan_decide` | `/campus/maintenance-plans/{id}/decision` | FacilityManage | CampusApprove (administrator) | n | y |
| `campus_reservation_decide` | `/campus/reservations/{id}/decision` | same slice manage permission | FacilityManage (administrator) | n | y |
| `campus_hostel_leave_decide` | `/campus/resident-leaves/{id}/decision` | same slice manage permission | HostelManage (administrator) | n | y |
| `campus_hostel_transfer_decide` | `/campus/room-transfers/{id}/decision` | HostelManage | CampusApprove (administrator) | n | y |
| `campus_transport_route_decide` | `/campus/transport-routes/{id}/decision` | TransportManage | CampusApprove (administrator) | n | y |
| `exam_conduct_decision` | `/exams/conduct-records/{id}/decision` | ExamSetupManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_policy_decision` | `/exams/exam-policies/{id}/decision` | ExamSetupManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_registration_decision` | `/exams/exam-registrations/{id}/decision` | ExamRegistrationManage, ExamSetupManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_schedule_decision` | `/exams/exam-schedules/{id}/decision` | ExamSetupManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_closure_decide` | `/exams/exam-sessions/{id}/closure/decision` | ResultManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_session_decision` | `/exams/exam-sessions/{id}/decision` | ExamSetupManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_malpractice_decide` | `/exams/malpractice-cases/{id}/decision` | same slice manage permission | MarksManage (administrator) | n | y |
| `exam_mark_sheet_decide` | `/exams/mark-sheets/{id}/decision` | MarksManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_moderation_decide` | `/exams/moderations/{id}/decision` | MarksManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_publication_authorise` | `/exams/publications/{id}/authorise` | ResultManage | ResultPublish (administrator, exam_controller) | n | y |
| `exam_registration_override_decision` | `/exams/registration-overrides/{id}/decision` | ExamRegistrationManage, ExamSetupManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_revaluation_decide` | `/exams/revaluation-requests/{id}/decision` | ResultManage | ExamApprove (administrator, exam_controller) | n | y |
| `exam_supplementary_authorise` | `/exams/supplementary-eligibility/{id}/authorise` | same slice manage permission | ResultManage (administrator) | n | y |
| `finance_aid_allocation_approve` | `/finance/aid-allocations/{id}/approve` | AidManage | FinanceApprove (administrator, finance_approver) | n | y |
| `finance_bank_account_decide` | `/finance/bank-accounts/{id}/decision` | TreasuryManage | FinanceApprove (administrator, finance_approver) | n | event only (no audit call found in slice) |
| `finance_dispute_decide` | `/finance/disputes/{id}/decision` | ChargeManage | FinanceApprove (administrator, finance_approver) | n | y |
| `finance_fee_structure_decide` | `/finance/fee-structures/{id}/decision` | ChargeManage | FinanceApprove (administrator, finance_approver) | n | y |
| `finance_journal_manual_decide` | `/finance/manual-journals/{id}/decision` | LedgerManage | FinanceApprove (administrator, finance_approver) | n | y |
| `academic_structure_decision` | `/institution/academic-structures/{structureId}/decision` | CurriculumManage | CurriculumPublish (administrator) | n | y |
| `academic_year_decision` | `/institution/academic-years/{yearId}/decision` | CalendarManage | CalendarPublish (administrator) | n | y |
| `calendar_exception_decision` | `/institution/calendar-exceptions/{exceptionId}/decision` | CalendarManage | CalendarPublish (administrator) | n | y |
| `calendar_simulation_decision` | `/institution/calendar-simulations/{simulationId}/decision` | CalendarManage | CalendarPublish (administrator) | n | y |
| `calendar_version_decision` | `/institution/calendar-versions/{versionId}/decision` | CalendarManage | CalendarPublish (administrator) | n | y |
| `campus_decision` | `/institution/campuses/{campusId}/decision` | same slice manage permission | CampusManage (administrator) | n | y |
| `curriculum_version_decision` | `/institution/curriculum-versions/{versionId}/decision` | CurriculumManage | CurriculumPublish (administrator) | n | y |
| `institution_decide_activation` | `/institution/institutions/{id}/decision` | InstitutionManage | InstitutionActivationDecide (administrator) | n | y |
| `org_restructure_decide` | `/institution/org-restructures/{id}/decision` | same slice manage permission | OrganisationManage (administrator) | n | y |
| `org_type_blueprint_decide` | `/institution/org-type-blueprints/{id}/decision` | same slice manage permission | OrganisationManage (administrator) | n | y |
| `school_model_decision` | `/institution/school-models/{schoolModelId}/decision` | CurriculumManage | CurriculumPublish (administrator) | n | y |
| `people_certificate_decide` | `/people/certificate-requests/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | event only (no audit call found in slice) |
| `people_engagement_decide` | `/people/engagements/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | y |
| `people_leave_adjustment_decide` | `/people/leave-adjustments/{id}/decision` | LeaveManage | PeopleApprove (administrator) | n | y |
| `people_leave_case_decide` | `/people/leave-cases/{id}/decision` | LeaveManage | PeopleApprove (administrator) | n | y |
| `people_leave_encashment_decide` | `/people/leave-encashments/{id}/decision` | LeaveManage | PeopleApprove (administrator) | n | y |
| `people_leave_policy_decide` | `/people/leave-policies/{id}/decision` | LeaveManage | PeopleApprove (administrator) | n | y |
| `people_leave_request_decide` | `/people/leave-requests/{id}/decision` | LeaveManage | LeaveDecide (administrator) | n | y |
| `people_leave_self_approval_decide` | `/people/me/leave-approvals/{id}/decision` | LeaveManage | PeopleSelfService (administrator) | n | y |
| `people_onboarding_decide` | `/people/onboarding-cases/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | event only (no audit call found in slice) |
| `people_payroll_config_decide` | `/people/payroll-configs/{id}/decision` | PayrollManage | PeopleApprove (administrator) | n | y |
| `people_position_assignment_decide` | `/people/position-assignments/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | event only (no audit call found in slice) |
| `people_probation_decide` | `/people/probation-reviews/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | event only (no audit call found in slice) |
| `people_recruitment_requisition_decide` | `/people/requisitions/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | event only (no audit call found in slice) |
| `people_separation_decide` | `/people/separation-cases/{id}/decision` | StaffManage | PeopleApprove (administrator) | n | event only (no audit call found in slice) |
| `people_workload_plan_decide` | `/people/workload-plans/{id}/decision` | LeaveManage | PeopleApprove (administrator) | n | y |
| `resources_asset_decide` | `/resources/assets/{id}/decision` | AssetManage | ResourcesApprove (administrator, procurement_approver) | n | event only (no audit call found in slice) |
| `resources_sourcing_decide` | `/resources/sourcing-events/{id}/decision` | ProcurementManage | ResourcesApprove (administrator, procurement_approver) | n | event only (no audit call found in slice) |
| `resources_stock_adjustment_decide` | `/resources/stock-adjustments/{id}/decision` | InventoryManage | ResourcesApprove (administrator, procurement_approver) | n | event only (no audit call found in slice) |
| `resources_stock_issue_decide` | `/resources/stock-issues/{id}/decision` | same slice manage permission | InventoryManage (administrator) | n | event only (no audit call found in slice) |
| `resources_supplier_decide` | `/resources/suppliers/{id}/decision` | ProcurementManage | ResourcesApprove (administrator, procurement_approver) | n | y |
| `services_accreditation_cycle_decide` | `/services/accreditation-cycles/{id}/decision` | AccreditationManage | ReportApprove (administrator) | n | y |
| `services_accreditation_framework_decide` | `/services/accreditation-frameworks/{id}/decision` | AccreditationManage | ReportApprove (administrator) | n | y |
| `services_accreditation_record_decide` | `/services/accreditation-records/{id}/decision` | AccreditationManage | ReportApprove (administrator) | n | y |
| `services_dashboard_decide` | `/services/dashboards/{id}/decision` | ReportManage | ReportApprove (administrator) | n | y |
| `services_regulatory_framework_decide` | `/services/reporting-frameworks/{id}/decision` | ReportManage | ReportApprove (administrator) | n | event only (no audit call found in slice) |
| `services_regulatory_package_decide` | `/services/submission-packages/{id}/decision` | ReportManage | ReportApprove (administrator) | n | event only (no audit call found in slice) |
| `alumni_transition_decision` | `/student/alumni-transitions/{id}/decision` | same slice manage permission | AlumniManage (administrator) | n | y |
| `conferred_qualification_decision` | `/student/conferred-qualifications/{id}/corrections/{correctionId}/decision` | CompletionManage | QualificationConfer (administrator) | n | y |
| `duplicate_case_decide` | `/student/duplicate-cases/{id}/decision` | same slice manage permission | StudentIdentityManage (administrator) | n | y |
| `exit_decision` | `/student/exits/{id}/decision` | CompletionManage | ExitDecide (administrator) | n | y |
| `guardian_relationship_verification_decision` | `/student/guardian-relationships/{id}/verification-decision` | same slice manage permission | GuardianManage (administrator) | n | y |
| `identity_correction_decide` | `/student/identity-corrections/{id}/decision` | same slice manage permission | StudentIdentityManage (administrator) | n | y |
| `qualification_completion_decision` | `/student/qualification-completions/{id}/decision` | CompletionManage | QualificationConfer (administrator) | n | y |
| `registration_decision` | `/student/registrations/{id}/decision` | same slice manage permission | RegistrationManage (administrator) | n | y |
| `scholarship_eligibility_recommendation_decision` | `/student/scholarship-eligibilities/{id}/recommendation-decision` | same slice manage permission | ScholarshipManage (administrator) | n | y |
| `scholarship_policy_decision` | `/student/scholarship-policies/{id}/decision` | ScholarshipManage | ScholarshipPublish (administrator) | n | y |
| `student_transfer_decide` | `/student/transfers/{id}/decision` | StudentManage | StudentRead (administrator) | n | y |
