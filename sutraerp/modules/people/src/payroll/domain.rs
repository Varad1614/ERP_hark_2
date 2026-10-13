//! Pure payroll rules of slice 7C (L2-E06): lifecycle transitions of the payroll aggregates,
//! configuration validation and the calculation engine (earnings, statutory deductions, employer
//! contributions, proration, loss of pay, leave encashment, retro deltas). No I/O — unit-tested.
//!
//! Money is `rust_decimal` end to end; each computed line is rounded **once** with the
//! configuration's rounding rule (`round2` = paise half away from zero, `rupee` = whole rupees).

use crate::shared::decimal::round2;
use chrono::{Datelike, NaiveDate};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use sutra_contracts::people::status::{
    CalculationBasis, PayComponentKind, PayrollAdjustmentStatus, PayrollConfigStatus,
    PayrollRunKind, PayrollRunStatus, PayslipStatus,
};
use utoipa::ToSchema;
use uuid::Uuid;

/// Declares a slice enum with `ALL`, `as_str`, `parse` and serde/OpenAPI `snake_case` names.
macro_rules! slice_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

slice_enum!(
    /// Statutory rule kinds (simplified governed rate + ceiling rules, contract §12).
    PayrollStatutoryKind {
        Pf => "pf",
        Esi => "esi",
        ProfessionalTax => "professional_tax",
        Tds => "tds",
    }
);

/// Short name used across the slice.
pub type StatutoryKind = PayrollStatutoryKind;

slice_enum!(
    /// Line rounding of a payroll configuration.
    PayrollRounding {
        Round2 => "round2",
        Rupee => "rupee",
    }
);

/// Short name used across the slice.
pub type Rounding = PayrollRounding;

slice_enum!(
    /// Separation final settlement (L3-E-056).
    FinalSettlementStatus {
        Initiated => "initiated",
        Calculated => "calculated",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Rejected => "rejected",
        Completed => "completed",
    }
);

slice_enum!(
    /// Where a payroll result line comes from.
    PayrollLineSource {
        Calculation => "calculation",
        Encashment => "encashment",
        Adjustment => "adjustment",
        Retro => "retro",
        OffCycle => "off_cycle",
        Recovery => "recovery",
    }
);

/// Short name used across the slice.
pub type LineSource = PayrollLineSource;

slice_enum!(
    /// Effective-dated salary assignment.
    SalaryAssignmentStatus {
        Active => "active",
        Ended => "ended",
    }
);

slice_enum!(
    /// Payroll period (month).
    PayrollPeriodStatus {
        Open => "open",
        Closed => "closed",
    }
);

/// Component code of basic pay (PF base, `percent_of_basic`, encashment rate).
pub const BASIC: &str = "BASIC";
/// Component code of dearness allowance (PF base and encashment rate when configured).
pub const DA: &str = "DA";
/// Line code of a leave encashment payment.
pub const LEAVE_ENCASHMENT: &str = "LEAVE_ENC";

// ------------------------------------------------------------------ transitions

/// Payroll configuration (L2-E06 §7.3): draft → review → approved → active → superseded →
/// archived; review → draft (rejected); an approved, never-activated version may be superseded.
pub fn config_can_transition(from: PayrollConfigStatus, to: PayrollConfigStatus) -> bool {
    use PayrollConfigStatus::*;
    matches!(
        (from, to),
        (Draft, Review)
            | (Review, Approved)
            | (Review, Draft)
            | (Approved, Active)
            | (Approved, Superseded)
            | (Active, Superseded)
            | (Superseded, Archived)
    )
}

/// Payroll run (L2-E06 §7.1).
pub fn run_can_transition(from: PayrollRunStatus, to: PayrollRunStatus) -> bool {
    use PayrollRunStatus::*;
    matches!(
        (from, to),
        (Created, Calculating)
            | (Calculating, Calculated)
            | (Calculating, Failed)
            | (Calculated, Calculating)
            | (Calculated, Validating)
            | (Calculated, Failed)
            | (Validating, PendingApproval)
            | (Validating, Failed)
            | (Validating, Calculating)
            | (PendingApproval, Approved)
            | (PendingApproval, Calculated)
            | (Approved, Finalized)
            | (Finalized, Posted)
            | (Posted, Closed)
            | (Failed, Reprocessing)
            | (Reprocessing, Calculating)
    )
}

/// Payroll adjustment (L2-E06 §7.3).
pub fn adjustment_can_transition(
    from: PayrollAdjustmentStatus,
    to: PayrollAdjustmentStatus,
) -> bool {
    use PayrollAdjustmentStatus::*;
    matches!(
        (from, to),
        (Requested, Validated)
            | (Requested, Rejected)
            | (Validated, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (Approved, Calculated)
            | (Calculated, Applied)
            | (Applied, Closed)
    )
}

/// Final settlement (L3-E-056).
pub fn settlement_can_transition(from: FinalSettlementStatus, to: FinalSettlementStatus) -> bool {
    use FinalSettlementStatus::*;
    matches!(
        (from, to),
        (Initiated, Calculated)
            | (Rejected, Calculated)
            | (Calculated, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (Approved, Completed)
    )
}

/// Payslip.
pub fn payslip_can_transition(from: PayslipStatus, to: PayslipStatus) -> bool {
    use PayslipStatus::*;
    matches!(
        (from, to),
        (Issued, Reissued) | (Issued, Withdrawn) | (Reissued, Reissued) | (Reissued, Withdrawn)
    )
}

/// Whether a run kind is calculated from the payroll population of its period.
pub fn kind_uses_population(kind: PayrollRunKind) -> bool {
    matches!(kind, PayrollRunKind::Regular | PayrollRunKind::Retro)
}

/// Statuses in which a run's results are final (payslips, adjustments, retro baselines).
pub fn run_is_final(s: PayrollRunStatus) -> bool {
    matches!(
        s,
        PayrollRunStatus::Finalized | PayrollRunStatus::Posted | PayrollRunStatus::Closed
    )
}

// ------------------------------------------------------------------ dates

/// First and last day of a month.
pub fn month_bounds(year: i32, month: u32) -> Option<(NaiveDate, NaiveDate)> {
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    Some((start, next.pred_opt()?))
}

/// Inclusive day count of `[from, to]` (0 when empty).
pub fn days_between(from: NaiveDate, to: NaiveDate) -> i64 {
    if to < from {
        0
    } else {
        (to - from).num_days() + 1
    }
}

/// First day of `[from, to]` that no already-paid period covers, walking forward from `from`
/// through the (possibly unordered, overlapping) `paid` periods; `None` when they cover the
/// whole window. A final settlement pays salary only from this day (no double pay of a month
/// the regular run already finalised).
pub fn first_unpaid_day(
    from: NaiveDate,
    to: NaiveDate,
    paid: &[(NaiveDate, NaiveDate)],
) -> Option<NaiveDate> {
    if to < from {
        return None;
    }
    let mut sorted: Vec<(NaiveDate, NaiveDate)> = paid.to_vec();
    sorted.sort();
    let mut cursor = from;
    for (s, e) in sorted {
        if s <= cursor && e >= cursor {
            cursor = e.succ_opt()?;
        }
        if cursor > to {
            return None;
        }
    }
    Some(cursor)
}

/// Days of `[from, to]` that none of the already-paid `paid` periods covers.
pub fn uncovered_days(from: NaiveDate, to: NaiveDate, paid: &[(NaiveDate, NaiveDate)]) -> i64 {
    let mut n = 0;
    let mut d = from;
    while d <= to {
        if !paid.iter().any(|(s, e)| *s <= d && d <= *e) {
            n += 1;
        }
        match d.succ_opt() {
            Some(x) => d = x,
            None => break,
        }
    }
    n
}

/// Days of `[period_start, period_end]` the engagement was employed: from its start (joiners)
/// to its exit / planned end (leavers).
pub fn employed_days(
    period_start: NaiveDate,
    period_end: NaiveDate,
    engagement_start: NaiveDate,
    engagement_end: Option<NaiveDate>,
) -> i64 {
    let from = period_start.max(engagement_start);
    let to = engagement_end.map_or(period_end, |e| e.min(period_end));
    days_between(from, to)
}

/// `YYYY-MM` of a date (7B's consumption posting key suffix).
pub fn year_month(d: NaiveDate) -> String {
    format!("{:04}-{:02}", d.year(), d.month())
}

// ------------------------------------------------------------------ configuration

/// One pay component of a configuration version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentDef {
    pub code: String,
    pub name: String,
    pub kind: PayComponentKind,
    pub basis: CalculationBasis,
    /// Default monthly amount (`fixed`).
    pub amount: Option<Decimal>,
    /// Default percentage (`percent_of_*`), e.g. `40.0000` = 40 %.
    pub rate: Option<Decimal>,
    pub sequence: i32,
}

/// A professional-tax slab: `amount` applies while gross ≤ `up_to` (`None` = no upper bound).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollTaxSlab {
    #[schema(value_type = Option<String>, example = "15000.00")]
    #[serde(with = "rust_decimal::serde::str_option", default)]
    pub up_to: Option<Decimal>,
    #[schema(value_type = String, example = "200.00")]
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
}

/// Short name used across the slice.
pub type Slab = PayrollTaxSlab;

/// One statutory rule of a configuration version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleDef {
    pub kind: StatutoryKind,
    pub employee_rate: Option<Decimal>,
    pub employer_rate: Option<Decimal>,
    pub wage_ceiling: Option<Decimal>,
    pub threshold: Option<Decimal>,
    pub slabs: Vec<Slab>,
    pub fixed_amount: Option<Decimal>,
    pub employee_code: String,
    pub employer_code: Option<String>,
}

/// One problem found by [`validate_config`]: `(field, code, message)`.
pub type Finding = (String, &'static str, String);

/// Review-time validation of a configuration version (L4-E-074-BH-002): a `BASIC` fixed
/// earning, earnings only `fixed` / `percent_of_basic`, statutory components produced by exactly
/// the rules that name them, complete rule parameters.
pub fn validate_config(components: &[ComponentDef], rules: &[RuleDef]) -> Vec<Finding> {
    let mut f: Vec<Finding> = Vec::new();
    let find = |code: &str| components.iter().find(|c| c.code == code);
    match find(BASIC) {
        Some(c) if c.kind == PayComponentKind::Earning && c.basis == CalculationBasis::Fixed => {}
        _ => f.push((
            "components".into(),
            "basic_required",
            "a BASIC earning with basis `fixed` is required".into(),
        )),
    }
    for c in components {
        let field = format!("components.{}", c.code);
        if c.kind == PayComponentKind::Earning
            && !matches!(
                c.basis,
                CalculationBasis::Fixed | CalculationBasis::PercentOfBasic
            )
        {
            f.push((
                field.clone(),
                "invalid_basis",
                "earnings are `fixed` or `percent_of_basic`".into(),
            ));
        }
        if c.basis == CalculationBasis::PercentOfBasic && c.code == BASIC {
            f.push((
                field.clone(),
                "invalid_basis",
                "BASIC cannot be a percentage of itself".into(),
            ));
        }
        if c.basis == CalculationBasis::Statutory {
            if c.kind == PayComponentKind::Earning {
                f.push((
                    field.clone(),
                    "invalid_basis",
                    "statutory components are deductions or employer contributions".into(),
                ));
            }
            let used = rules.iter().any(|r| {
                r.employee_code == c.code || r.employer_code.as_deref() == Some(c.code.as_str())
            });
            if !used {
                f.push((
                    field,
                    "rule_missing",
                    "no statutory rule computes this component".into(),
                ));
            }
        }
    }
    for r in rules {
        let field = format!("statutoryRules.{}", r.kind);
        let ee = find(&r.employee_code);
        if !ee.is_some_and(|c| {
            c.basis == CalculationBasis::Statutory && c.kind == PayComponentKind::Deduction
        }) {
            f.push((
                field.clone(),
                "component_missing",
                format!(
                    "employee component {} must be a statutory deduction",
                    r.employee_code
                ),
            ));
        }
        if let Some(code) = &r.employer_code {
            if !find(code).is_some_and(|c| {
                c.basis == CalculationBasis::Statutory
                    && c.kind == PayComponentKind::EmployerContribution
            }) {
                f.push((
                    field.clone(),
                    "component_missing",
                    format!("employer component {code} must be a statutory employer contribution"),
                ));
            }
        }
        let missing = match r.kind {
            StatutoryKind::Pf => r.employee_rate.is_none(),
            StatutoryKind::Esi => r.employee_rate.is_none() || r.threshold.is_none(),
            StatutoryKind::ProfessionalTax => r.slabs.is_empty(),
            StatutoryKind::Tds => r.fixed_amount.is_none() && r.employee_rate.is_none(),
        };
        if missing {
            f.push((
                field.clone(),
                "incomplete_rule",
                match r.kind {
                    StatutoryKind::Pf => "PF needs an employee rate",
                    StatutoryKind::Esi => "ESI needs an employee rate and a gross threshold",
                    StatutoryKind::ProfessionalTax => "professional tax needs slabs",
                    StatutoryKind::Tds => "TDS needs a monthly amount or a rate",
                }
                .into(),
            ));
        }
        if r.employer_code.is_some() && r.employer_rate.is_none() {
            f.push((
                field,
                "incomplete_rule",
                "an employer component needs an employer rate".into(),
            ));
        }
    }
    f
}

// ------------------------------------------------------------------ calculation

/// One salary structure line (amount for `fixed`, rate for `percent_of_*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureLine {
    pub code: String,
    pub amount: Option<Decimal>,
    pub rate: Option<Decimal>,
}

/// A computed result line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalcLine {
    pub code: String,
    pub name: String,
    pub kind: PayComponentKind,
    pub amount: Decimal,
    pub basis: Option<String>,
    pub source: LineSource,
    pub source_ref: Option<Uuid>,
}

impl CalcLine {
    pub fn new(code: &str, name: &str, kind: PayComponentKind, amount: Decimal) -> Self {
        Self {
            code: code.to_owned(),
            name: name.to_owned(),
            kind,
            amount,
            basis: None,
            source: LineSource::Calculation,
            source_ref: None,
        }
    }

    fn basis(mut self, b: &str) -> Self {
        self.basis = Some(b.to_owned());
        self
    }

    pub fn source(mut self, s: LineSource, r: Option<Uuid>) -> Self {
        self.source = s;
        self.source_ref = r;
        self
    }
}

/// One encashment ledger entry to pay: `(entry id, days)`.
pub type Encashment = (Uuid, Decimal);

/// Inputs of one employee calculation.
#[derive(Clone, Debug)]
pub struct CalcInput<'a> {
    pub components: &'a [ComponentDef],
    pub rules: &'a [RuleDef],
    pub structure: &'a [StructureLine],
    pub rounding: Rounding,
    pub encashment_divisor: i32,
    pub period_days: i64,
    pub employed_days: i64,
    /// Loss of pay (unpaid leave + unauthorised absence) from `LeaveReader::payroll_inputs`.
    pub lop_days: Decimal,
    pub encashments: &'a [Encashment],
}

/// Totals of a set of lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub gross: Decimal,
    pub deductions: Decimal,
    pub employer: Decimal,
    pub net: Decimal,
}

/// Sums lines by kind; `net = gross − deductions`.
pub fn totals(lines: &[CalcLine]) -> Totals {
    let mut t = Totals::default();
    for l in lines {
        match l.kind {
            PayComponentKind::Earning => t.gross += l.amount,
            PayComponentKind::Deduction => t.deductions += l.amount,
            PayComponentKind::EmployerContribution => t.employer += l.amount,
        }
    }
    t.net = t.gross - t.deductions;
    t
}

/// Output of [`calculate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalcOutput {
    pub lines: Vec<CalcLine>,
    pub payable_days: Decimal,
    pub totals: Totals,
    /// Business findings (`negative_net_pay`, …) that block validation.
    pub exceptions: Vec<String>,
}

/// Rounds one computed amount with the configuration's rule.
pub fn round_with(r: Rounding, v: Decimal) -> Decimal {
    match r {
        Rounding::Round2 => round2(v),
        Rounding::Rupee => {
            let mut x = v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
            x.rescale(2);
            x
        }
    }
}

fn pct(base: Decimal, rate: Decimal) -> Decimal {
    base * rate / Decimal::from(100)
}

/// Calculates one employee's monthly payroll (L3-E-053): earnings prorated by payable days
/// (employed days − loss of pay) over calendar days, leave encashment at (BASIC + DA) / divisor
/// per day, non-statutory deductions / employer contributions, then the statutory rules (PF on
/// earned BASIC + DA up to the wage ceiling, ESI while gross ≤ threshold, professional tax by
/// slab, TDS as a monthly amount or a rate). Each line is rounded once.
pub fn calculate(i: &CalcInput<'_>) -> CalcOutput {
    let r = i.rounding;
    let employed = Decimal::from(i.employed_days.max(0));
    let payable = (employed - i.lop_days).max(Decimal::ZERO).min(employed);
    let factor = if i.period_days > 0 {
        payable / Decimal::from(i.period_days)
    } else {
        Decimal::ZERO
    };
    let line_of = |code: &str| i.structure.iter().find(|l| l.code == code);
    let amount_of = |c: &ComponentDef| line_of(&c.code).and_then(|l| l.amount).or(c.amount);
    let rate_of = |c: &ComponentDef| line_of(&c.code).and_then(|l| l.rate).or(c.rate);
    let mut ordered: Vec<&ComponentDef> = i.components.iter().collect();
    ordered.sort_by(|a, b| a.sequence.cmp(&b.sequence).then(a.code.cmp(&b.code)));

    let basic_monthly = i
        .components
        .iter()
        .find(|c| c.code == BASIC)
        .and_then(amount_of)
        .unwrap_or(Decimal::ZERO);
    let mut monthly: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut earned: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut lines: Vec<CalcLine> = Vec::new();
    let mut exceptions: Vec<String> = Vec::new();

    // Earnings (prorated).
    for c in ordered
        .iter()
        .copied()
        .filter(|c| c.kind == PayComponentKind::Earning)
    {
        let m = match c.basis {
            CalculationBasis::Fixed => amount_of(c).unwrap_or(Decimal::ZERO),
            CalculationBasis::PercentOfBasic => {
                pct(basic_monthly, rate_of(c).unwrap_or(Decimal::ZERO))
            }
            _ => {
                exceptions.push(format!("component {} has an invalid earning basis", c.code));
                continue;
            }
        };
        monthly.insert(c.code.clone(), m);
        let a = round_with(r, m * factor);
        if a.is_zero() {
            continue;
        }
        earned.insert(c.code.clone(), a);
        lines.push(CalcLine::new(&c.code, &c.name, c.kind, a).basis(c.basis.as_str()));
    }
    let regular_gross: Decimal = earned.values().copied().sum();
    let earned_of = |code: &str| earned.get(code).copied().unwrap_or(Decimal::ZERO);

    // Leave encashment (7B `encashment` entries, each paid once).
    let day_rate = (monthly.get(BASIC).copied().unwrap_or(Decimal::ZERO)
        + monthly.get(DA).copied().unwrap_or(Decimal::ZERO))
        / Decimal::from(i.encashment_divisor.max(1));
    for (entry, days) in i.encashments {
        let a = round_with(r, *days * day_rate);
        if a.is_zero() {
            continue;
        }
        lines.push(
            CalcLine::new(
                LEAVE_ENCASHMENT,
                "Leave encashment",
                PayComponentKind::Earning,
                a,
            )
            .basis("encashment")
            .source(LineSource::Encashment, Some(*entry)),
        );
    }

    // Non-statutory deductions and employer contributions.
    for c in ordered
        .iter()
        .copied()
        .filter(|c| c.kind != PayComponentKind::Earning)
    {
        let a = match c.basis {
            // Fixed deductions / contributions (loan instalments, welfare funds) apply in a
            // month with earnings only.
            CalculationBasis::Fixed if regular_gross > Decimal::ZERO => {
                amount_of(c).unwrap_or(Decimal::ZERO)
            }
            CalculationBasis::Fixed => Decimal::ZERO,
            CalculationBasis::PercentOfBasic => {
                pct(earned_of(BASIC), rate_of(c).unwrap_or(Decimal::ZERO))
            }
            CalculationBasis::PercentOfGross => {
                pct(regular_gross, rate_of(c).unwrap_or(Decimal::ZERO))
            }
            CalculationBasis::Statutory => continue,
        };
        let a = round_with(r, a);
        if !a.is_zero() {
            lines.push(CalcLine::new(&c.code, &c.name, c.kind, a).basis(c.basis.as_str()));
        }
    }

    // Statutory rules.
    let name_of = |code: &str| {
        i.components
            .iter()
            .find(|c| c.code == code)
            .map_or_else(|| code.to_owned(), |c| c.name.clone())
    };
    let mut push = |code: &str, kind: PayComponentKind, raw: Decimal, basis: &str| {
        let a = round_with(r, raw);
        if !a.is_zero() {
            lines.push(CalcLine::new(code, &name_of(code), kind, a).basis(basis));
        }
    };
    let mut rules: Vec<&RuleDef> = i.rules.iter().collect();
    rules.sort_by_key(|x| StatutoryKind::ALL.iter().position(|k| *k == x.kind));
    for rule in rules {
        let er_code = rule.employer_code.as_deref();
        match rule.kind {
            StatutoryKind::Pf => {
                let mut base = earned_of(BASIC) + earned_of(DA);
                if let Some(ceiling) = rule.wage_ceiling {
                    base = base.min(ceiling);
                }
                push(
                    &rule.employee_code,
                    PayComponentKind::Deduction,
                    pct(base, rule.employee_rate.unwrap_or_default()),
                    "statutory:pf",
                );
                if let Some(code) = er_code {
                    push(
                        code,
                        PayComponentKind::EmployerContribution,
                        pct(base, rule.employer_rate.unwrap_or_default()),
                        "statutory:pf",
                    );
                }
            }
            StatutoryKind::Esi => {
                if rule.threshold.is_none_or(|t| regular_gross <= t) {
                    push(
                        &rule.employee_code,
                        PayComponentKind::Deduction,
                        pct(regular_gross, rule.employee_rate.unwrap_or_default()),
                        "statutory:esi",
                    );
                    if let Some(code) = er_code {
                        push(
                            code,
                            PayComponentKind::EmployerContribution,
                            pct(regular_gross, rule.employer_rate.unwrap_or_default()),
                            "statutory:esi",
                        );
                    }
                }
            }
            StatutoryKind::ProfessionalTax => {
                if regular_gross > Decimal::ZERO {
                    let slab = rule
                        .slabs
                        .iter()
                        .find(|s| s.up_to.is_none_or(|u| regular_gross <= u));
                    if let Some(s) = slab {
                        push(
                            &rule.employee_code,
                            PayComponentKind::Deduction,
                            s.amount,
                            "statutory:professional_tax",
                        );
                    }
                }
            }
            StatutoryKind::Tds => {
                if regular_gross > Decimal::ZERO {
                    let a = rule.fixed_amount.unwrap_or_else(|| {
                        pct(regular_gross, rule.employee_rate.unwrap_or_default())
                    });
                    push(
                        &rule.employee_code,
                        PayComponentKind::Deduction,
                        a,
                        "statutory:tds",
                    );
                }
            }
        }
    }

    let t = totals(&lines);
    if t.net < Decimal::ZERO {
        exceptions.push("negative_net_pay".into());
    }
    CalcOutput {
        lines,
        payable_days: payable,
        totals: t,
        exceptions,
    }
}

/// A line of an earlier, finalised result: `(code, name, kind, amount)`.
pub type PriorLine = (String, String, PayComponentKind, Decimal);

/// Retro arrears (L3-E-055): per `(code, kind)` the recalculated amount minus what finalised
/// results of the period already paid; only non-zero differences become `retro` lines.
pub fn retro_delta(recalculated: &[CalcLine], prior: &[PriorLine]) -> Vec<CalcLine> {
    let mut sums: BTreeMap<(String, &'static str), (String, PayComponentKind, Decimal)> =
        BTreeMap::new();
    for l in recalculated {
        let e = sums.entry((l.code.clone(), l.kind.as_str())).or_insert((
            l.name.clone(),
            l.kind,
            Decimal::ZERO,
        ));
        e.2 += l.amount;
    }
    for (code, name, kind, amount) in prior {
        let e = sums.entry((code.clone(), kind.as_str())).or_insert((
            name.clone(),
            *kind,
            Decimal::ZERO,
        ));
        e.2 -= *amount;
    }
    sums.into_iter()
        .filter(|(_, (_, _, d))| !d.is_zero())
        .map(|((code, _), (name, kind, d))| {
            CalcLine::new(&code, &name, kind, d)
                .basis("retro")
                .source(LineSource::Retro, None)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }

    #[test]
    fn first_unpaid_day_skips_finalised_periods() {
        let (from, to) = (day("2026-11-01"), day("2026-11-15"));
        assert_eq!(first_unpaid_day(from, to, &[]), Some(from));
        // The regular run of November paid the whole month → nothing left to pay.
        assert_eq!(
            first_unpaid_day(from, to, &[(day("2026-11-01"), day("2026-11-30"))]),
            None
        );
        // A half-month period paid → only the rest is unpaid.
        assert_eq!(
            first_unpaid_day(from, to, &[(day("2026-11-01"), day("2026-11-07"))]),
            Some(day("2026-11-08"))
        );
        // Unordered, chained periods.
        assert_eq!(
            first_unpaid_day(
                from,
                to,
                &[
                    (day("2026-11-08"), day("2026-11-10")),
                    (day("2026-10-01"), day("2026-11-07")),
                ]
            ),
            Some(day("2026-11-11"))
        );
        // A period that starts after `from` leaves `from` unpaid.
        assert_eq!(
            first_unpaid_day(from, to, &[(day("2026-11-05"), day("2026-11-30"))]),
            Some(from)
        );
        // Empty window.
        assert_eq!(first_unpaid_day(to, from, &[]), None);
        assert_eq!(uncovered_days(from, to, &[]), 15);
        assert_eq!(
            uncovered_days(from, to, &[(day("2026-11-01"), day("2026-11-30"))]),
            0
        );
        assert_eq!(
            uncovered_days(from, to, &[(day("2026-11-05"), day("2026-11-09"))]),
            10
        );
        assert_eq!(uncovered_days(to, from, &[]), 0);
    }

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn comp(
        code: &str,
        kind: PayComponentKind,
        basis: CalculationBasis,
        amount: Option<&str>,
        rate: Option<&str>,
        seq: i32,
    ) -> ComponentDef {
        ComponentDef {
            code: code.into(),
            name: code.into(),
            kind,
            basis,
            amount: amount.map(d),
            rate: rate.map(d),
            sequence: seq,
        }
    }

    fn standard() -> (Vec<ComponentDef>, Vec<RuleDef>) {
        use CalculationBasis::*;
        use PayComponentKind::*;
        let components = vec![
            comp(BASIC, Earning, Fixed, None, None, 10),
            comp(DA, Earning, PercentOfBasic, None, Some("10"), 20),
            comp("HRA", Earning, PercentOfBasic, None, Some("40"), 30),
            comp("PF", Deduction, Statutory, None, None, 100),
            comp("PF_ER", EmployerContribution, Statutory, None, None, 110),
            comp("PT", Deduction, Statutory, None, None, 120),
            comp("TDS", Deduction, Statutory, None, None, 130),
        ];
        let rules = vec![
            RuleDef {
                kind: StatutoryKind::Pf,
                employee_rate: Some(d("12")),
                employer_rate: Some(d("12")),
                wage_ceiling: Some(d("15000")),
                threshold: None,
                slabs: vec![],
                fixed_amount: None,
                employee_code: "PF".into(),
                employer_code: Some("PF_ER".into()),
            },
            RuleDef {
                kind: StatutoryKind::ProfessionalTax,
                employee_rate: None,
                employer_rate: None,
                wage_ceiling: None,
                threshold: None,
                slabs: vec![
                    Slab {
                        up_to: Some(d("15000")),
                        amount: d("0"),
                    },
                    Slab {
                        up_to: None,
                        amount: d("200"),
                    },
                ],
                fixed_amount: None,
                employee_code: "PT".into(),
                employer_code: None,
            },
            RuleDef {
                kind: StatutoryKind::Tds,
                employee_rate: None,
                employer_rate: None,
                wage_ceiling: None,
                threshold: None,
                slabs: vec![],
                fixed_amount: Some(d("1000")),
                employee_code: "TDS".into(),
                employer_code: None,
            },
        ];
        (components, rules)
    }

    fn structure() -> Vec<StructureLine> {
        vec![StructureLine {
            code: BASIC.into(),
            amount: Some(d("30000")),
            rate: None,
        }]
    }

    fn amount(o: &CalcOutput, code: &str) -> Decimal {
        o.lines
            .iter()
            .filter(|l| l.code == code)
            .map(|l| l.amount)
            .sum()
    }

    fn input<'a>(
        c: &'a [ComponentDef],
        r: &'a [RuleDef],
        s: &'a [StructureLine],
        days: (i64, i64),
        lop: &str,
        enc: &'a [Encashment],
    ) -> CalcInput<'a> {
        CalcInput {
            components: c,
            rules: r,
            structure: s,
            rounding: Rounding::Round2,
            encashment_divisor: 30,
            period_days: days.0,
            employed_days: days.1,
            lop_days: d(lop),
            encashments: enc,
        }
    }

    #[test]
    fn full_month_with_statutory_deductions() {
        let (c, r) = standard();
        let s = structure();
        let o = calculate(&input(&c, &r, &s, (30, 30), "0", &[]));
        assert_eq!(amount(&o, BASIC), d("30000.00"));
        assert_eq!(amount(&o, DA), d("3000.00"));
        assert_eq!(amount(&o, "HRA"), d("12000.00"));
        // PF base = min(33000, 15000) → 1800 each side.
        assert_eq!(amount(&o, "PF"), d("1800.00"));
        assert_eq!(amount(&o, "PF_ER"), d("1800.00"));
        assert_eq!(amount(&o, "PT"), d("200"));
        assert_eq!(amount(&o, "TDS"), d("1000"));
        assert_eq!(o.totals.gross, d("45000.00"));
        assert_eq!(o.totals.deductions, d("3000.00"));
        assert_eq!(o.totals.employer, d("1800.00"));
        assert_eq!(o.totals.net, d("42000.00"));
        assert!(o.exceptions.is_empty());
    }

    #[test]
    fn loss_of_pay_prorates_and_encashment_pays_per_day() {
        let (c, r) = standard();
        let s = structure();
        let entry = Uuid::nil();
        let enc = [(entry, d("3"))];
        let o = calculate(&input(&c, &r, &s, (30, 30), "2.5", &enc));
        assert_eq!(o.payable_days, d("27.5"));
        assert_eq!(amount(&o, BASIC), d("27500.00"));
        assert_eq!(amount(&o, DA), d("2750.00"));
        // (30000 + 3000) / 30 × 3 = 3300
        assert_eq!(amount(&o, LEAVE_ENCASHMENT), d("3300.00"));
        assert!(o
            .lines
            .iter()
            .any(|l| l.source == LineSource::Encashment && l.source_ref == Some(entry)));
    }

    #[test]
    fn joiners_are_prorated_and_rupee_rounding_applies() {
        let (c, r) = standard();
        let s = structure();
        let mut i = input(&c, &r, &s, (31, 10), "0", &[]);
        i.rounding = Rounding::Rupee;
        let o = calculate(&i);
        // 30000 × 10 / 31 = 9677.419… → 9677
        assert_eq!(amount(&o, BASIC), d("9677"));
        // Nothing earned → no TDS / PT; a zero month never goes negative.
        let none = calculate(&input(&c, &r, &s, (30, 30), "31", &[]));
        assert_eq!(none.totals, Totals::default());
        assert!(none.exceptions.is_empty());
    }

    #[test]
    fn fixed_deductions_can_make_net_negative() {
        let (mut c, r) = standard();
        c.push(comp(
            "LOAN",
            PayComponentKind::Deduction,
            CalculationBasis::Fixed,
            Some("50000"),
            None,
            200,
        ));
        let s = structure();
        let o = calculate(&input(&c, &r, &s, (30, 30), "0", &[]));
        assert!(o.exceptions.contains(&"negative_net_pay".to_owned()));
    }

    #[test]
    fn config_validation_finds_gaps() {
        let (c, r) = standard();
        assert!(
            validate_config(&c, &r).is_empty(),
            "{:?}",
            validate_config(&c, &r)
        );
        let f = validate_config(&c[1..], &r[..1]);
        let codes: Vec<&str> = f.iter().map(|x| x.1).collect();
        assert!(codes.contains(&"basic_required"));
        assert!(codes.contains(&"rule_missing"), "PT/TDS have no rule");
        let mut bad = r.clone();
        bad[0].employee_rate = None;
        assert!(validate_config(&c, &bad)
            .iter()
            .any(|x| x.1 == "incomplete_rule"));
    }

    #[test]
    fn retro_deltas_only_carry_differences() {
        let new = vec![
            CalcLine::new(BASIC, "Basic", PayComponentKind::Earning, d("33000")),
            CalcLine::new("PF", "PF", PayComponentKind::Deduction, d("1800")),
        ];
        let prior = vec![
            (
                BASIC.to_owned(),
                "Basic".to_owned(),
                PayComponentKind::Earning,
                d("30000"),
            ),
            (
                "PF".to_owned(),
                "PF".to_owned(),
                PayComponentKind::Deduction,
                d("1800"),
            ),
        ];
        let delta = retro_delta(&new, &prior);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].amount, d("3000"));
        assert_eq!(delta[0].source, LineSource::Retro);
        assert_eq!(totals(&delta).net, d("3000"));
    }

    #[test]
    fn transitions_and_dates() {
        use PayrollRunStatus::*;
        assert!(run_can_transition(Created, Calculating));
        assert!(run_can_transition(Failed, Reprocessing));
        assert!(!run_can_transition(Created, Finalized));
        assert!(!run_can_transition(Finalized, Calculating));
        assert!(config_can_transition(
            PayrollConfigStatus::Approved,
            PayrollConfigStatus::Active
        ));
        assert!(!config_can_transition(
            PayrollConfigStatus::Draft,
            PayrollConfigStatus::Active
        ));
        assert!(settlement_can_transition(
            FinalSettlementStatus::Rejected,
            FinalSettlementStatus::Calculated
        ));
        assert!(!payslip_can_transition(
            PayslipStatus::Withdrawn,
            PayslipStatus::Issued
        ));
        assert!(adjustment_can_transition(
            PayrollAdjustmentStatus::Calculated,
            PayrollAdjustmentStatus::Applied
        ));
        assert!(kind_uses_population(PayrollRunKind::Retro));
        assert!(run_is_final(Posted));
        let (s, e) = month_bounds(2026, 2).unwrap_or_default();
        assert_eq!(days_between(s, e), 28);
        let (s, e) = month_bounds(2026, 12).unwrap_or_default();
        assert_eq!(e.day(), 31);
        let mid = NaiveDate::from_ymd_opt(2026, 12, 15).unwrap_or_default();
        assert_eq!(employed_days(s, e, mid, None), 17);
        assert_eq!(employed_days(s, e, s, Some(mid)), 15);
        assert_eq!(year_month(mid), "2026-12");
        assert_eq!(
            StatutoryKind::parse("professional_tax"),
            Some(StatutoryKind::ProfessionalTax)
        );
        assert_eq!(round_with(Rounding::Rupee, d("10.5")).to_string(), "11.00");
    }
}
