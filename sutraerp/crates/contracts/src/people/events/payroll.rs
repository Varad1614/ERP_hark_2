//! **7C** — payroll configuration, runs, results, Finance posting, payslips, adjustments, retro pay, final settlement, off-cycle (L2-E06; L3-E-053…057, 074).
//!
//! Owned by slice 7C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch7-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;

event_type!(
    /// Payroll configuration version active (L3-E-074).
    PAYROLL_CONFIG_ACTIVATED, PAYROLL_CONFIG_ACTIVATED_SCHEMA, "payroll_config.activated");
event_type!(
    /// Payroll run calculated (L3-E-053).
    PAYROLL_RUN_CALCULATED, PAYROLL_RUN_CALCULATED_SCHEMA, "payroll_run.calculated");
event_type!(
    /// Payroll run submitted for approval (L3-E-053).
    PAYROLL_RUN_SUBMITTED, PAYROLL_RUN_SUBMITTED_SCHEMA, "payroll_run.submitted");
event_type!(
    /// Payroll run approved (L3-E-053).
    PAYROLL_RUN_APPROVED, PAYROLL_RUN_APPROVED_SCHEMA, "payroll_run.approved");
event_type!(
    /// Payroll run finalised: results immutable (L3-E-053).
    PAYROLL_RUN_FINALIZED, PAYROLL_RUN_FINALIZED_SCHEMA, "payroll_run.finalized");
event_type!(
    /// Payroll liability posted to Finance ([`super::PayrollPostedV1`]; L3-E-053).
    PAYROLL_RUN_POSTED, PAYROLL_RUN_POSTED_SCHEMA, "payroll_run.posted");
event_type!(
    /// Payroll calculation / validation failed (L3-E-053).
    PAYROLL_RUN_FAILED, PAYROLL_RUN_FAILED_SCHEMA, "payroll_run.failed");
event_type!(
    /// Payslip issued to the staff member (L3-E-053).
    PAYSLIP_ISSUED, PAYSLIP_ISSUED_SCHEMA, "payslip.issued");
event_type!(
    /// Payroll adjustment applied as a corrective outcome (L3-E-054/055).
    PAYROLL_ADJUSTMENT_APPLIED, PAYROLL_ADJUSTMENT_APPLIED_SCHEMA, "payroll_adjustment.applied");
event_type!(
    /// Separation final settlement completed (L3-E-056).
    FINAL_SETTLEMENT_COMPLETED, FINAL_SETTLEMENT_COMPLETED_SCHEMA, "final_settlement.completed");

/// Event types of this slice (inbox rules may claim only these).
pub const EVENT_TYPES: &[&str] = &[
    PAYROLL_CONFIG_ACTIVATED,
    PAYROLL_RUN_CALCULATED,
    PAYROLL_RUN_SUBMITTED,
    PAYROLL_RUN_APPROVED,
    PAYROLL_RUN_FINALIZED,
    PAYROLL_RUN_POSTED,
    PAYROLL_RUN_FAILED,
    PAYSLIP_ISSUED,
    PAYROLL_ADJUSTMENT_APPLIED,
    FINAL_SETTLEMENT_COMPLETED,
];
