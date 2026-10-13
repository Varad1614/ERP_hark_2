//! Read ports over the shared People tables (Batch 7 foundation, `docs/batch7-contracts.md` §6.2).
//!
//! * [`WorkforceDirectory`] — "7A → 7B / 7C": staff records and engagements (foundation tables
//!   `people.staff_member` / `people.staff_engagement`). Implemented by
//!   `sutra_people::PgWorkforceDirectory`.
//! * [`LeaveReader`] — "7B → 7C": leave balances and payroll inputs projected from the
//!   append-only `people.leave_ledger_entry` and `people.attendance_day`. Implemented by
//!   `sutra_people::PgLeaveReader`.
//!
//! Tenant always comes from `scope`; another tenant's rows are indistinguishable from missing.

use crate::people::ids::{LeaveLedgerEntryId, StaffEngagementId, StaffMemberId};
use crate::people::status::{EmploymentType, EngagementStatus, StaffCategory, StaffStatus};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// A staff record (canonical workforce member + core profile).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaffSummary {
    pub id: StaffMemberId,
    pub institution_id: InstitutionId,
    /// `EMP-000001`.
    pub staff_number: String,
    /// Login subject (Keycloak `sub`, dev `dev|<loginId>`), when linked.
    pub subject_ref: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: String,
    pub work_email: Option<String>,
    pub staff_category: StaffCategory,
    pub status: StaffStatus,
    /// Canonical record of a merged duplicate (L3-E-009).
    pub canonical_staff_id: Option<StaffMemberId>,
    pub version: i64,
}

/// One engagement (employment) of a staff member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngagementSummary {
    pub id: StaffEngagementId,
    pub staff_id: StaffMemberId,
    pub institution_id: InstitutionId,
    /// `ENG-000001`.
    pub engagement_number: String,
    /// Domain B org unit (department), when known.
    pub org_unit_id: Option<Uuid>,
    /// Domain B campus, when known.
    pub campus_id: Option<Uuid>,
    pub staff_category: StaffCategory,
    pub employment_type: EmploymentType,
    pub designation: String,
    /// Pay level / grade code (7C maps it to a salary structure).
    pub grade: Option<String>,
    pub start_date: NaiveDate,
    /// Planned end (contract).
    pub end_date: Option<NaiveDate>,
    /// Last working day (exit; set by 7A before closing).
    pub last_working_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    /// Reporting manager (7A L3-E-018; 7B routes manager approvals with it).
    pub reports_to_staff_id: Option<StaffMemberId>,
    /// Engagement this one rejoins (L3-E-008).
    pub rejoin_of_engagement_id: Option<StaffEngagementId>,
    pub status: EngagementStatus,
    pub version: i64,
}

/// Read port over staff records and engagements ("7A → 7B / 7C").
#[async_trait::async_trait]
pub trait WorkforceDirectory: Send + Sync + 'static {
    async fn staff(
        &self,
        scope: &ExecutionScope,
        id: StaffMemberId,
    ) -> Result<Option<StaffSummary>, KernelError>;

    /// The **live** (not merged / archived) staff record linked to a login subject.
    async fn staff_by_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StaffSummary>, KernelError>;

    /// Staff records of an institution (optionally one status), by staff number.
    async fn staff_of_institution(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        status: Option<StaffStatus>,
    ) -> Result<Vec<StaffSummary>, KernelError>;

    async fn engagement(
        &self,
        scope: &ExecutionScope,
        id: StaffEngagementId,
    ) -> Result<Option<EngagementSummary>, KernelError>;

    /// The live engagement of a staff member, else the most recently started one.
    async fn current_engagement(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
    ) -> Result<Option<EngagementSummary>, KernelError>;

    /// Every engagement of a staff member, newest first.
    async fn engagements_of_staff(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
    ) -> Result<Vec<EngagementSummary>, KernelError>;

    /// Payroll population of a period: engagements whose status `is_payable()` and that started
    /// on or before `to` (and did not end before `from`), plus engagements `closed` with a
    /// `last_working_date` inside `[from, to]`. Ordered by engagement number.
    async fn payable_engagements(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EngagementSummary>, KernelError>;

    /// Live engagements whose reporting manager is `manager` (7B manager approvals).
    async fn reports_of(
        &self,
        scope: &ExecutionScope,
        manager: StaffMemberId,
    ) -> Result<Vec<EngagementSummary>, KernelError>;
}

/// Leave balance of one leave type and year, projected from the ledger (days, two places).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveBalance {
    pub leave_type_id: Uuid,
    pub leave_type_code: String,
    pub leave_year: i32,
    /// Opening + accrual + carry forward + adjustments (net of their reversals).
    pub entitled: Decimal,
    /// Outstanding reservations (reserved − released).
    pub reserved: Decimal,
    /// Consumption (net of reversals).
    pub consumed: Decimal,
    /// Σ of every entry: what may still be requested.
    pub available: Decimal,
}

/// One `encashment` ledger entry (positive days encashed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncashmentEntry {
    pub entry_id: LeaveLedgerEntryId,
    pub leave_type_id: Uuid,
    pub leave_type_code: String,
    pub days: Decimal,
    pub effective_on: NaiveDate,
}

/// Leave and attendance facts payroll needs for one staff member and period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeavePayrollInputs {
    pub staff_id: StaffMemberId,
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Consumed days of unpaid (`is_paid = false`) leave effective in the period, net of reversals.
    pub unpaid_leave_days: Decimal,
    /// Unauthorised absence from attendance days in the period (absent 1, half day 0.5).
    pub absent_days: Decimal,
    /// `encashment` entries effective in the period (7C pays each once).
    pub encashments: Vec<EncashmentEntry>,
}

impl LeavePayrollInputs {
    /// Loss-of-pay days: unpaid leave + unauthorised absence.
    pub fn lop_days(&self) -> Decimal {
        self.unpaid_leave_days + self.absent_days
    }
}

/// Read port over the leave ledger and attendance days ("7B → 7C").
#[async_trait::async_trait]
pub trait LeaveReader: Send + Sync + 'static {
    /// Balances of every leave type the staff member has entries for in `leave_year`.
    async fn balances(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
        leave_year: i32,
    ) -> Result<Vec<LeaveBalance>, KernelError>;

    /// Payroll inputs of `[from, to]` (inclusive).
    async fn payroll_inputs(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<LeavePayrollInputs, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn lop_days_add_unpaid_leave_and_absence() {
        let i = LeavePayrollInputs {
            staff_id: StaffMemberId(Uuid::nil()),
            from: NaiveDate::default(),
            to: NaiveDate::default(),
            unpaid_leave_days: Decimal::from_str("2.00").unwrap_or_default(),
            absent_days: Decimal::from_str("1.50").unwrap_or_default(),
            encashments: vec![],
        };
        assert_eq!(i.lop_days(), Decimal::from_str("3.50").unwrap_or_default());
    }
}
