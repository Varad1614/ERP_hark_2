//! Data migration import / export port (Part 2 · S4-import, `docs/readiness/part2/s4-import.md`).
//!
//! The Platform module (Domain A) owns the **job** — upload, column mapping, saved mapping
//! profiles, dry run, duplicate detection, error report, commit, approval, reconciliation and
//! audit (`platform.import_*` tables). It never writes another domain's tables: each owning
//! module implements [`ImportTargetPort`] for the entities it owns and the composition root
//! registers the adapters.
//!
//! Contract of [`ImportTargetPort::apply`]:
//!
//! * one owner-local transaction per [`ImportBatch`]; every row runs inside its own savepoint
//!   through **the same domain validation and writers the module's API uses** (domain
//!   validators, reference checks, numbering, lifecycle, events, audit, database constraints);
//! * `dry_run = true` → the transaction is always rolled back (nothing persists, nothing is
//!   published), so a dry run reports exactly what a commit would do;
//! * `dry_run = false` → committed only when **every** row succeeded; one failing row rolls the
//!   whole batch back (`committed = false`) and its outcome carries the errors;
//! * idempotent by natural key: a row whose natural key already exists is skipped
//!   ([`OnExisting::Skip`]) or updated in place ([`OnExisting::Update`], where the entity
//!   supports it) — never duplicated.

use rust_decimal::Decimal;
use std::collections::BTreeMap;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// Entity keys (stable wire values of `entity`).
pub mod entity {
    pub const STUDENTS: &str = "students";
    pub const STAFF: &str = "staff";
    pub const CAMPUSES: &str = "campuses";
    pub const ORG_UNITS: &str = "org_units";
    pub const PROGRAMMES: &str = "programmes";
    pub const CURRICULA: &str = "curricula";
    pub const FEE_HEADS: &str = "fee_heads";
    pub const OPENING_BALANCES: &str = "opening_balances";
    pub const HISTORICAL_RESULTS: &str = "historical_results";

    /// Every entity, in the order the admin screen lists them (dependency order of a migration).
    pub const ALL: &[&str] = &[
        CAMPUSES,
        ORG_UNITS,
        PROGRAMMES,
        CURRICULA,
        STAFF,
        STUDENTS,
        FEE_HEADS,
        OPENING_BALANCES,
        HISTORICAL_RESULTS,
    ];
}

/// Value type of a column (drives template examples, the mapping screen and pre-validation).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    Text,
    /// Upper-case code (`A-Z0-9_-`).
    Code,
    /// ISO date `YYYY-MM-DD` (XLSX date cells are converted).
    Date,
    /// Decimal amount, at most two decimals.
    Decimal,
    Integer,
    Email,
    Phone,
    /// `true` / `false` (also `yes` / `no`, `1` / `0`).
    Boolean,
    /// One of [`ColumnSpec::choices`].
    Choice,
}

/// One column of an entity's import file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    /// Canonical column name (template header, mapping target).
    pub name: &'static str,
    pub label: &'static str,
    pub required: bool,
    pub kind: ColumnKind,
    pub choices: &'static [&'static str],
    pub example: &'static str,
    pub help: &'static str,
}

impl ColumnSpec {
    pub const fn new(name: &'static str, label: &'static str, kind: ColumnKind) -> Self {
        Self {
            name,
            label,
            required: false,
            kind,
            choices: &[],
            example: "",
            help: "",
        }
    }

    pub const fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub const fn example(mut self, example: &'static str) -> Self {
        self.example = example;
        self
    }

    pub const fn choices(mut self, choices: &'static [&'static str]) -> Self {
        self.choices = choices;
        self
    }

    pub const fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }
}

/// What an entity's import looks like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitySpec {
    pub entity: &'static str,
    pub label: &'static str,
    /// Owning domain module (`student`, `people`, `institution`, `finance`).
    pub owner: &'static str,
    pub columns: Vec<ColumnSpec>,
    /// Columns whose (normalised) values identify a record: in-file duplicate detection and the
    /// skip / update decision against the database.
    pub natural_key: &'static [&'static str],
    /// The money column reconciled against the ledger (opening balances).
    pub amount_column: Option<&'static str>,
    /// Rows sharing this column's value must land in the same batch (a curriculum's slots).
    pub group_by: Option<&'static str>,
    /// `true` when re-importing an existing natural key may update it ([`OnExisting::Update`]).
    pub supports_update: bool,
    /// `true` when a commit needs a second person's approval (separation of duties).
    pub requires_approval: bool,
    /// The owning domain's manage permission, required in addition to `platform.import.manage`.
    pub permission: Permission,
}

impl EntitySpec {
    pub fn column(&self, name: &str) -> Option<&ColumnSpec> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// What to do with a row whose natural key already exists.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OnExisting {
    /// Leave the existing record untouched (the default; makes re-imports no-ops).
    #[default]
    Skip,
    /// Overwrite the existing record's importable fields.
    Update,
}

impl OnExisting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Update => "update",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skip" => Some(Self::Skip),
            "update" => Some(Self::Update),
            _ => None,
        }
    }
}

/// One mapped row: canonical column name → cell text (trimmed; empty cells absent).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportRow {
    /// 1-based line of the source file (header = line 1).
    pub line: u32,
    pub values: BTreeMap<String, String>,
}

impl ImportRow {
    /// Trimmed, non-empty value of `column`.
    pub fn get(&self, column: &str) -> Option<&str> {
        self.values
            .get(column)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }
}

/// One batch handed to an adapter.
#[derive(Clone, Debug)]
pub struct ImportBatch {
    pub job_id: Uuid,
    pub institution_id: InstitutionId,
    pub on_existing: OnExisting,
    pub dry_run: bool,
    /// User who runs the import (audit / history actor).
    pub actor: Option<String>,
    pub rows: Vec<ImportRow>,
}

/// What happened (or, in a dry run, would happen) to a row.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RowAction {
    Create,
    Update,
    /// Natural key already present; left untouched.
    Skip,
    Error,
}

impl RowAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Skip => "skip",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Self::Create),
            "update" => Some(Self::Update),
            "skip" => Some(Self::Skip),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Outcome of one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowOutcome {
    pub line: u32,
    pub action: RowAction,
    /// The created / updated / existing record.
    pub record_id: Option<Uuid>,
    /// Money the row moved (opening balances), for reconciliation.
    pub amount: Option<Decimal>,
    pub errors: Vec<FieldViolation>,
}

impl RowOutcome {
    pub fn error(line: u32, errors: Vec<FieldViolation>) -> Self {
        Self {
            line,
            action: RowAction::Error,
            record_id: None,
            amount: None,
            errors,
        }
    }

    pub fn done(line: u32, action: RowAction, record_id: Uuid) -> Self {
        Self {
            line,
            action,
            record_id: Some(record_id),
            amount: None,
            errors: vec![],
        }
    }
}

/// Outcome of one batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchOutcome {
    pub rows: Vec<RowOutcome>,
    /// `true` only for a non-dry-run batch whose rows all succeeded.
    pub committed: bool,
}

/// An importable / exportable entity, implemented by the module that owns it.
#[async_trait::async_trait]
pub trait ImportTargetPort: Send + Sync + 'static {
    fn spec(&self) -> EntitySpec;

    /// Normalised natural key of a row (`None` when a key column is empty — the row then fails
    /// the required-column check).
    fn natural_key(&self, row: &ImportRow) -> Option<String> {
        let spec = self.spec();
        let mut parts = Vec::with_capacity(spec.natural_key.len());
        for c in spec.natural_key {
            parts.push(row.get(c)?.to_lowercase());
        }
        Some(parts.join("|"))
    }

    /// Validates and (unless `dry_run`) writes one batch — see the module docs.
    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError>;

    /// Current records of the institution as rows of the import columns (round-trippable).
    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError>;

    /// Ledger total of the given records (journal debits of the opening-balance postings), for
    /// the reconciliation summary. `None` for entities without money.
    async fn ledger_total(
        &self,
        _scope: &ExecutionScope,
        _institution_id: InstitutionId,
        _record_ids: &[Uuid],
    ) -> Result<Option<Decimal>, KernelError> {
        Ok(None)
    }
}

/// Parses a cell as an ISO date.
pub fn parse_date(column: &str, raw: &str) -> Result<chrono::NaiveDate, FieldViolation> {
    chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| FieldViolation::new(column, "date", "must be a date in YYYY-MM-DD form"))
}

/// Parses a cell as a decimal with at most two places (`1,25,000.50` grouping allowed).
pub fn parse_amount(column: &str, raw: &str) -> Result<Decimal, FieldViolation> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, ',' | '₹' | ' '))
        .collect();
    match cleaned.parse::<Decimal>() {
        Ok(d) if d.scale() <= 2 => Ok(d),
        _ => Err(FieldViolation::new(
            column,
            "amount",
            "must be a number with at most two decimals",
        )),
    }
}

/// Parses a cell as an integer.
pub fn parse_int(column: &str, raw: &str) -> Result<i32, FieldViolation> {
    raw.trim()
        .parse::<i32>()
        .map_err(|_| FieldViolation::new(column, "integer", "must be a whole number"))
}

/// Parses a cell as a boolean (`true/false`, `yes/no`, `y/n`, `1/0`).
pub fn parse_bool(column: &str, raw: &str) -> Result<bool, FieldViolation> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" => Ok(true),
        "false" | "no" | "n" | "0" => Ok(false),
        _ => Err(FieldViolation::new(
            column,
            "boolean",
            "must be true/false (yes/no)",
        )),
    }
}

/// The violations of a row-level error (validation keeps its fields; conflicts / not-found
/// become one row-level message). `None` for infrastructure failures that must abort the batch.
pub fn row_errors(e: &KernelError) -> Option<Vec<FieldViolation>> {
    Some(match e {
        KernelError::Validation(v) => v.clone(),
        KernelError::Conflict(m) => vec![FieldViolation::new("", "conflict", m.clone())],
        KernelError::NotFound { entity } => vec![FieldViolation::new(
            "",
            "not_found",
            format!("{entity} not found"),
        )],
        KernelError::BadRequest(m) => vec![FieldViolation::new("", "invalid", m.clone())],
        KernelError::VersionConflict { .. } => vec![FieldViolation::new(
            "",
            "conflict",
            "the record changed concurrently",
        )],
        KernelError::Forbidden(m) | KernelError::Scope(m) => {
            vec![FieldViolation::new("", "forbidden", m.clone())]
        }
        _ => return None,
    })
}

/// Maps per-item results of a batch run to row outcomes (`Ok` = the row's outcome).
pub fn outcomes(
    lines: &[u32],
    results: Vec<Result<RowOutcome, KernelError>>,
) -> Result<Vec<RowOutcome>, KernelError> {
    let mut out = Vec::with_capacity(results.len());
    for (line, r) in lines.iter().zip(results) {
        match r {
            Ok(o) => out.push(o),
            Err(e) => match row_errors(&e) {
                Some(v) => out.push(RowOutcome::error(*line, v)),
                None => return Err(e),
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_parse_strictly() {
        assert_eq!(
            parse_amount("amount", "1,25,000.50").unwrap(),
            "125000.50".parse::<Decimal>().unwrap()
        );
        assert!(parse_amount("amount", "12.345").is_err());
        assert!(parse_amount("amount", "abc").is_err());
        assert!(parse_date("d", "2026-06-15").is_ok());
        assert!(parse_date("d", "15/06/2026").is_err());
        assert!(parse_bool("b", "Yes").unwrap());
        assert!(!parse_bool("b", "0").unwrap());
        assert!(parse_bool("b", "maybe").is_err());
        assert_eq!(parse_int("n", " 3 ").unwrap(), 3);
    }

    #[test]
    fn row_values_are_trimmed_and_empty_is_absent() {
        let mut r = ImportRow::default();
        r.values.insert("a".into(), "  x ".into());
        r.values.insert("b".into(), "   ".into());
        assert_eq!(r.get("a"), Some("x"));
        assert_eq!(r.get("b"), None);
        assert_eq!(r.get("c"), None);
    }

    #[test]
    fn entity_keys_are_unique() {
        let mut v = entity::ALL.to_vec();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), entity::ALL.len());
        assert_eq!(OnExisting::parse("update"), Some(OnExisting::Update));
        assert_eq!(
            RowAction::parse(RowAction::Skip.as_str()),
            Some(RowAction::Skip)
        );
    }

    #[test]
    fn infrastructure_errors_abort_instead_of_failing_a_row() {
        assert!(row_errors(&KernelError::Unavailable("db".into())).is_none());
        assert_eq!(
            row_errors(&KernelError::Conflict("dup".into())).unwrap()[0].code,
            "conflict"
        );
        let o = outcomes(
            &[2, 3],
            vec![
                Ok(RowOutcome::done(2, RowAction::Create, Uuid::nil())),
                Err(KernelError::Conflict("x".into())),
            ],
        )
        .unwrap();
        assert_eq!(o[1].line, 3);
        assert_eq!(o[1].action, RowAction::Error);
        assert!(outcomes(&[2], vec![Err(KernelError::Unavailable("db".into()))]).is_err());
    }
}
