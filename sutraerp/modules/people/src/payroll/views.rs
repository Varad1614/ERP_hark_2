//! Wire views and request bodies of the payroll API (money / days / rates as strings). Schema
//! names carry a payroll prefix so they never collide with other modules' OpenAPI schemas.

use super::adjust::stored_lines;
use super::common::{fmt4, opt2, opt_money, rate};
use super::domain::{
    ComponentDef, FinalSettlementStatus, PayrollPeriodStatus, PayrollRounding,
    PayrollStatutoryKind, PayrollTaxSlab, RuleDef, SalaryAssignmentStatus, StructureLine,
};
use super::rows::*;
use crate::shared::decimal::fmt2;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sutra_contracts::people::status::{
    CalculationBasis, PayComponentKind, PayrollAdjustmentStatus, PayrollConfigStatus,
    PayrollResultStatus, PayrollRunKind, PayrollRunStatus,
};
use sutra_kernel::error::KernelError;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// A pay component of a configuration version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayComponentView {
    #[schema(example = "BASIC")]
    pub code: String,
    pub name: String,
    pub kind: PayComponentKind,
    pub basis: CalculationBasis,
    #[schema(example = "30000.00")]
    pub amount: Option<String>,
    #[schema(example = "40.0000")]
    pub rate: Option<String>,
    pub sequence: i32,
}

impl From<&ComponentDef> for PayComponentView {
    fn from(c: &ComponentDef) -> Self {
        Self {
            code: c.code.clone(),
            name: c.name.clone(),
            kind: c.kind,
            basis: c.basis,
            amount: opt2(c.amount),
            rate: c.rate.map(fmt4),
            sequence: c.sequence,
        }
    }
}

/// A statutory rule of a configuration version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatutoryRuleView {
    pub kind: PayrollStatutoryKind,
    #[schema(example = "12.0000")]
    pub employee_rate: Option<String>,
    pub employer_rate: Option<String>,
    #[schema(example = "15000.00")]
    pub wage_ceiling: Option<String>,
    pub threshold: Option<String>,
    pub slabs: Vec<PayrollTaxSlab>,
    pub fixed_amount: Option<String>,
    pub employee_component_code: String,
    pub employer_component_code: Option<String>,
}

impl From<&RuleDef> for StatutoryRuleView {
    fn from(r: &RuleDef) -> Self {
        Self {
            kind: r.kind,
            employee_rate: r.employee_rate.map(fmt4),
            employer_rate: r.employer_rate.map(fmt4),
            wage_ceiling: opt2(r.wage_ceiling),
            threshold: opt2(r.threshold),
            slabs: r.slabs.clone(),
            fixed_amount: opt2(r.fixed_amount),
            employee_component_code: r.employee_code.clone(),
            employer_component_code: r.employer_code.clone(),
        }
    }
}

/// A payroll configuration version (components and rules on the detail only).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollConfigView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub version_no: i32,
    pub name: String,
    pub effective_from: NaiveDate,
    pub rounding: PayrollRounding,
    pub encashment_divisor: i32,
    pub copied_from_id: Option<Uuid>,
    pub status: PayrollConfigStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub activated_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub components: Vec<PayComponentView>,
    pub statutory_rules: Vec<StatutoryRuleView>,
    pub version: i64,
}

impl PayrollConfigView {
    pub fn of(c: &ConfigRow, comps: &[ComponentRow], rules: &[RuleRow]) -> Self {
        Self {
            id: c.id,
            institution_id: c.institution_id.as_uuid(),
            version_no: c.version_no,
            name: c.name.clone(),
            effective_from: c.effective_from,
            rounding: c.rounding,
            encashment_divisor: c.encashment_divisor,
            copied_from_id: c.copied_from_id,
            status: c.status,
            status_reason: c.status_reason.clone(),
            process_instance_id: c.process_instance_id.clone(),
            approved_at: c.approved_at,
            activated_at: c.activated_at,
            superseded_at: c.superseded_at,
            components: comps
                .iter()
                .map(|x| PayComponentView::from(&x.def))
                .collect(),
            statutory_rules: rules
                .iter()
                .map(|x| StatutoryRuleView::from(&x.def))
                .collect(),
            version: c.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayrollConfigList {
    pub items: Vec<PayrollConfigView>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PayrollConfigQuery {
    /// Status wire name.
    pub status: Option<String>,
}

/// New configuration version.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollConfigCreate {
    pub name: String,
    pub effective_from: NaiveDate,
    pub rounding: Option<PayrollRounding>,
    /// Leave encashment day rate divisor (default 30).
    pub encashment_divisor: Option<i32>,
    /// Copy components and statutory rules from this version.
    pub copy_from: Option<Uuid>,
}

/// A pay component (added or replaced by code on a draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayComponentInput {
    pub code: String,
    pub name: String,
    pub kind: PayComponentKind,
    pub basis: CalculationBasis,
    #[schema(example = "30000.00")]
    pub amount: Option<String>,
    #[schema(example = "40")]
    pub rate: Option<String>,
    pub sequence: Option<i32>,
}

/// A statutory rule (added or replaced by kind on a draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatutoryRuleInput {
    pub kind: PayrollStatutoryKind,
    pub employee_rate: Option<String>,
    pub employer_rate: Option<String>,
    pub wage_ceiling: Option<String>,
    pub threshold: Option<String>,
    pub slabs: Option<Vec<PayrollTaxSlab>>,
    pub fixed_amount: Option<String>,
    pub employee_component_code: String,
    pub employer_component_code: Option<String>,
}

/// A salary structure line.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryStructureLine {
    pub component_code: String,
    #[schema(example = "30000.00")]
    pub amount: Option<String>,
    pub rate: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryStructureView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub grade: Option<String>,
    pub status: String,
    pub lines: Vec<SalaryStructureLine>,
    pub version: i64,
}

impl From<&StructureRow> for SalaryStructureView {
    fn from(s: &StructureRow) -> Self {
        Self {
            id: s.id,
            institution_id: s.institution_id.as_uuid(),
            code: s.code.clone(),
            name: s.name.clone(),
            grade: s.grade.clone(),
            status: s.status.clone(),
            lines: s
                .lines
                .iter()
                .map(|l| SalaryStructureLine {
                    component_code: l.code.clone(),
                    amount: opt2(l.amount),
                    rate: l.rate.map(fmt4),
                })
                .collect(),
            version: s.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SalaryStructureList {
    pub items: Vec<SalaryStructureView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryStructureCreate {
    #[schema(example = "AP-1")]
    pub code: String,
    pub name: String,
    /// Engagement grade this structure pays.
    pub grade: Option<String>,
    pub lines: Vec<SalaryStructureLine>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryStructureUpdate {
    pub name: String,
    pub grade: Option<String>,
    pub lines: Vec<SalaryStructureLine>,
}

/// Parses structure lines of a request.
pub fn structure_lines_in(
    lines: &[SalaryStructureLine],
) -> Result<Vec<StructureLine>, KernelError> {
    lines
        .iter()
        .map(|l| {
            Ok(StructureLine {
                code: l.component_code.trim().to_owned(),
                amount: opt_money("lines.amount", l.amount.as_deref())?,
                rate: rate("lines.rate", l.rate.as_deref())?,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryAssignmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub structure_id: Uuid,
    pub structure_code: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub status: SalaryAssignmentStatus,
    pub reason: Option<String>,
    pub version: i64,
}

impl From<&AssignmentRow> for SalaryAssignmentView {
    fn from(a: &AssignmentRow) -> Self {
        Self {
            id: a.id,
            institution_id: a.institution_id.as_uuid(),
            staff_id: a.staff_id,
            engagement_id: a.engagement_id,
            structure_id: a.structure_id,
            structure_code: a.structure_code.clone(),
            effective_from: a.effective_from,
            effective_to: a.effective_to,
            status: a.status,
            reason: a.reason.clone(),
            version: a.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SalaryAssignmentList {
    pub items: Vec<SalaryAssignmentView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryAssignmentCreate {
    /// Defaults to the active structure of the engagement's grade.
    pub structure_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SalaryAssignmentEnd {
    pub end_date: NaiveDate,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollPeriodView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub year: i32,
    pub month: i32,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: PayrollPeriodStatus,
    pub version: i64,
}

impl From<&PeriodRow> for PayrollPeriodView {
    fn from(p: &PeriodRow) -> Self {
        Self {
            id: p.id,
            institution_id: p.institution_id.as_uuid(),
            year: p.year,
            month: p.month,
            period_start: p.period_start,
            period_end: p.period_end,
            status: p.status,
            version: p.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayrollPeriodList {
    pub items: Vec<PayrollPeriodView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PayrollPeriodCreate {
    pub year: i32,
    pub month: u32,
}

/// A business exception found by calculation (blocks validation).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollException {
    pub engagement_id: Option<Uuid>,
    pub staff_id: Option<Uuid>,
    pub staff_number: Option<String>,
    pub code: String,
    pub message: String,
}

/// Decodes stored exceptions (objects or plain codes).
pub fn exceptions_of(v: &serde_json::Value) -> Vec<PayrollException> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    let s = |k: &str| x.get(k).and_then(serde_json::Value::as_str);
                    PayrollException {
                        engagement_id: s("engagementId").and_then(|v| v.parse().ok()),
                        staff_id: s("staffId").and_then(|v| v.parse().ok()),
                        staff_number: s("staffNumber").map(str::to_owned),
                        code: s("code").or(x.as_str()).unwrap_or("exception").to_owned(),
                        message: s("message").or(x.as_str()).unwrap_or("").to_owned(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// An off-cycle item.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollOffCycleItem {
    pub engagement_id: Uuid,
    #[schema(example = "BONUS")]
    pub component_code: String,
    #[schema(example = "5000.00")]
    pub amount: String,
    pub note: Option<String>,
}

/// A payroll run (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollRunView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "PR-000001")]
    pub number: String,
    pub period_id: Uuid,
    pub kind: PayrollRunKind,
    pub status: PayrollRunStatus,
    pub status_reason: Option<String>,
    pub description: Option<String>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub headcount: i32,
    #[schema(example = "45000.00")]
    pub gross_earnings: String,
    pub employee_deductions: String,
    pub employer_contributions: String,
    pub net_pay: String,
    pub config_id: Option<Uuid>,
    pub items: Vec<PayrollOffCycleItem>,
    pub exceptions: Vec<PayrollException>,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub calculated_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub posted_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl From<&RunRow> for PayrollRunView {
    fn from(r: &RunRow) -> Self {
        Self {
            id: r.id,
            institution_id: r.institution_id.as_uuid(),
            number: r.number.clone(),
            period_id: r.period_id,
            kind: r.kind,
            status: r.status,
            status_reason: r.status_reason.clone(),
            description: r.description.clone(),
            period_start: r.period_start,
            period_end: r.period_end,
            headcount: r.headcount,
            gross_earnings: fmt2(r.gross_earnings),
            employee_deductions: fmt2(r.employee_deductions),
            employer_contributions: fmt2(r.employer_contributions),
            net_pay: fmt2(r.net_pay),
            config_id: r.config_id,
            items: serde_json::from_value(r.items.clone()).unwrap_or_default(),
            exceptions: exceptions_of(&r.exceptions),
            journal_entry_id: r.journal_entry_id,
            journal_entry_number: r.journal_entry_number.clone(),
            process_instance_id: r.process_instance_id.clone(),
            calculated_at: r.calculated_at,
            approved_at: r.approved_at,
            finalized_at: r.finalized_at,
            posted_at: r.posted_at,
            closed_at: r.closed_at,
            version: r.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayrollRunList {
    pub items: Vec<PayrollRunView>,
}

/// New run of a period.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollRunCreate {
    pub kind: PayrollRunKind,
    pub description: Option<String>,
    /// Off-cycle runs only.
    pub items: Option<Vec<PayrollOffCycleItem>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollResultLine {
    pub line_no: i32,
    pub code: String,
    pub name: String,
    pub kind: PayComponentKind,
    #[schema(example = "30000.00")]
    pub amount: String,
    pub basis: Option<String>,
    /// `calculation`, `encashment`, `adjustment`, `retro`, `off_cycle`, `recovery`.
    pub source: String,
    pub source_ref: Option<Uuid>,
}

impl From<&LineRow> for PayrollResultLine {
    fn from(l: &LineRow) -> Self {
        Self {
            line_no: l.line_no,
            code: l.code.clone(),
            name: l.name.clone(),
            kind: l.kind,
            amount: fmt2(l.amount),
            basis: l.basis.clone(),
            source: l.source.clone(),
            source_ref: l.source_ref,
        }
    }
}

/// A per-employee payroll result with its lines (immutable once finalised).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollResultView {
    pub id: Uuid,
    pub run_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub status: PayrollResultStatus,
    pub revision: i32,
    pub supersedes_result_id: Option<Uuid>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    #[schema(example = "30.00")]
    pub period_days: String,
    pub employed_days: String,
    pub lop_days: String,
    pub payable_days: String,
    pub gross_earnings: String,
    pub total_deductions: String,
    pub employer_contributions: String,
    pub net_pay: String,
    pub salary_assignment_id: Option<Uuid>,
    pub structure_id: Option<Uuid>,
    pub config_id: Option<Uuid>,
    /// Calculation snapshot: configuration version, rules, structure, inputs.
    #[schema(value_type = Object)]
    pub snapshot: serde_json::Value,
    pub exceptions: Vec<PayrollException>,
    pub lines: Vec<PayrollResultLine>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl From<&ResultRow> for PayrollResultView {
    fn from(r: &ResultRow) -> Self {
        Self {
            id: r.id,
            run_id: r.run_id,
            staff_id: r.staff_id,
            engagement_id: r.engagement_id,
            staff_number: r.staff_number.clone(),
            display_name: r.display_name.clone(),
            status: r.status,
            revision: r.revision,
            supersedes_result_id: r.supersedes_result_id,
            period_start: r.period_start,
            period_end: r.period_end,
            period_days: fmt2(r.period_days),
            employed_days: fmt2(r.employed_days),
            lop_days: fmt2(r.lop_days),
            payable_days: fmt2(r.payable_days),
            gross_earnings: fmt2(r.gross_earnings),
            total_deductions: fmt2(r.total_deductions),
            employer_contributions: fmt2(r.employer_contributions),
            net_pay: fmt2(r.net_pay),
            salary_assignment_id: r.salary_assignment_id,
            structure_id: r.structure_id,
            config_id: r.config_id,
            snapshot: r.snapshot.clone(),
            exceptions: exceptions_of(&r.exceptions),
            lines: r.lines.iter().map(PayrollResultLine::from).collect(),
            finalized_at: r.finalized_at,
            version: r.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollResultList {
    pub items: Vec<PayrollResultView>,
    /// Staff number to continue after (`?cursor=`).
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct PayrollResultQuery {
    /// Include superseded revisions.
    pub include_superseded: Option<bool>,
    /// At most 200 (default 50).
    pub limit: Option<i64>,
    /// Staff number to continue after.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollAdjustmentLine {
    pub code: String,
    pub name: Option<String>,
    pub kind: Option<PayComponentKind>,
    /// Signed delta (`"-500.00"`).
    #[schema(example = "1500.00")]
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollAdjustmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub result_id: Uuid,
    pub run_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub status: PayrollAdjustmentStatus,
    pub status_reason: Option<String>,
    pub reason: String,
    pub lines: Vec<PayrollAdjustmentLine>,
    pub gross_delta: String,
    pub deduction_delta: String,
    pub employer_delta: String,
    pub net_delta: String,
    pub revised_result_id: Option<Uuid>,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl From<&AdjustmentRow> for PayrollAdjustmentView {
    fn from(a: &AdjustmentRow) -> Self {
        Self {
            id: a.id,
            institution_id: a.institution_id.as_uuid(),
            result_id: a.result_id,
            run_id: a.run_id,
            staff_id: a.staff_id,
            engagement_id: a.engagement_id,
            status: a.status,
            status_reason: a.status_reason.clone(),
            reason: a.reason.clone(),
            lines: stored_lines(&a.lines)
                .iter()
                .map(|l| PayrollAdjustmentLine {
                    code: l.code.clone(),
                    name: Some(l.name.clone()),
                    kind: Some(l.kind),
                    amount: fmt2(l.amount),
                })
                .collect(),
            gross_delta: fmt2(a.gross_delta),
            deduction_delta: fmt2(a.deduction_delta),
            employer_delta: fmt2(a.employer_delta),
            net_delta: fmt2(a.net_delta),
            revised_result_id: a.revised_result_id,
            journal_entry_id: a.journal_entry_id,
            journal_entry_number: a.journal_entry_number.clone(),
            process_instance_id: a.process_instance_id.clone(),
            applied_at: a.applied_at,
            version: a.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayrollAdjustmentList {
    pub items: Vec<PayrollAdjustmentView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PayrollAdjustmentCreate {
    pub reason: String,
    pub lines: Vec<PayrollAdjustmentLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SettlementRecovery {
    pub code: String,
    pub name: String,
    #[schema(example = "5000.00")]
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FinalSettlementView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    #[schema(example = "FNF-000001")]
    pub number: Option<String>,
    pub status: FinalSettlementStatus,
    pub status_reason: Option<String>,
    pub last_working_date: NaiveDate,
    pub recoveries: Vec<SettlementRecovery>,
    pub notes: Option<String>,
    pub run_id: Option<Uuid>,
    pub salary_amount: String,
    pub encashment_amount: String,
    pub encashment_days: String,
    pub recovery_amount: String,
    pub net_pay: String,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl From<&SettlementRow> for FinalSettlementView {
    fn from(s: &SettlementRow) -> Self {
        Self {
            id: s.id,
            institution_id: s.institution_id.as_uuid(),
            staff_id: s.staff_id,
            engagement_id: s.engagement_id,
            number: s.number.clone(),
            status: s.status,
            status_reason: s.status_reason.clone(),
            last_working_date: s.last_working_date,
            recoveries: serde_json::from_value(s.recoveries.clone()).unwrap_or_default(),
            notes: s.notes.clone(),
            run_id: s.run_id,
            salary_amount: fmt2(s.salary_amount),
            encashment_amount: fmt2(s.encashment_amount),
            encashment_days: fmt2(s.encashment_days),
            recovery_amount: fmt2(s.recovery_amount),
            net_pay: fmt2(s.net_pay),
            journal_entry_id: s.journal_entry_id,
            journal_entry_number: s.journal_entry_number.clone(),
            process_instance_id: s.process_instance_id.clone(),
            completed_at: s.completed_at,
            version: s.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FinalSettlementList {
    pub items: Vec<FinalSettlementView>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct FinalSettlementCreate {
    pub recoveries: Option<Vec<SettlementRecovery>>,
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exceptions_decode_objects_and_strings() {
        let v = serde_json::json!([
            {"staffNumber": "EMP-000001", "code": "missing_salary_assignment", "message": "m"},
            "negative_net_pay"
        ]);
        let e = exceptions_of(&v);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].code, "missing_salary_assignment");
        assert_eq!(e[1].code, "negative_net_pay");
    }
}
