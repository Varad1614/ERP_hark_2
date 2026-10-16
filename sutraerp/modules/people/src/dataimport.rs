//! Part 2 · S4-import: Domain E import target `staff` (`docs/readiness/part2/s4-import.md`).
//!
//! Every row goes through 7A's [`create_in`] — the same profile validation, duplicate work
//! e-mail check, staff numbering (`EMP-…`), `staff.created` event and audit as
//! `POST /people/institutions/{id}/staff` — inside the batch transaction
//! ([`sutra_persistence::batch::run`]). Natural key: the work e-mail (a live record with the
//! same e-mail is skipped, never duplicated).

use crate::shared::guard::require_institution_writable;
use crate::shared::{Actor, PeopleState};
use crate::workforce::staff::{create_in, StaffCreate};
use chrono::NaiveDate;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::status::{Gender, StaffCategory, StaffStatus};
use sutra_contracts::platform::dataimport::{
    entity, outcomes, parse_date, BatchOutcome, ColumnKind, ColumnSpec, EntitySpec, ImportBatch,
    ImportRow, ImportTargetPort, OnExisting, RowAction, RowOutcome,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::batch::{self, ItemWriter};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const CATEGORIES: &[&str] = &["teaching", "non_teaching", "administrative"];
const GENDERS: &[&str] = &["female", "male", "other", "undisclosed"];
const STATUSES: &[&str] = &["draft", "pending_verification", "active"];

/// `staff` import target.
#[derive(Clone)]
pub struct PgStaffImport {
    state: PeopleState,
}

impl PgStaffImport {
    pub fn new(state: PeopleState) -> Self {
        Self { state }
    }
}

/// 7A names API members in camelCase; the import reports file columns.
fn column_of(field: &str) -> &str {
    match field {
        "givenName" => "given_name",
        "familyName" => "family_name",
        "displayName" => "display_name",
        "workEmail" => "work_email",
        "dateOfBirth" => "date_of_birth",
        "staffCategory" => "staff_category",
        other => other,
    }
}

fn staff_body(row: &ImportRow) -> Result<StaffCreate, KernelError> {
    let mut v = Vec::new();
    let mut choice = |c: &str, choices: &[&str]| -> Option<String> {
        let raw = row.get(c)?;
        if choices.contains(&raw.to_lowercase().as_str()) {
            Some(raw.to_lowercase())
        } else {
            v.push(FieldViolation::new(
                c,
                "choice",
                format!("{c} must be one of {}", choices.join(", ")),
            ));
            None
        }
    };
    let category = choice("staff_category", CATEGORIES);
    let gender = choice("gender", GENDERS);
    let status = choice("status", STATUSES);
    if row.get("staff_category").is_none() {
        v.push(FieldViolation::new(
            "staff_category",
            "required",
            "staff_category is required",
        ));
    }
    if row.get("work_email").is_none() {
        v.push(FieldViolation::new(
            "work_email",
            "required",
            "work_email is required (it identifies the record on re-import)",
        ));
    }
    let date_of_birth = match row.get("date_of_birth").map(|d| parse_date("date_of_birth", d)) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => None,
    };
    if !v.is_empty() {
        return Err(KernelError::Validation(v));
    }
    let staff_category: StaffCategory = category
        .as_deref()
        .and_then(StaffCategory::parse)
        .ok_or_else(|| KernelError::BadRequest("staff_category".into()))?;
    let initial_status: StaffStatus = status
        .as_deref()
        .and_then(StaffStatus::parse)
        .unwrap_or(StaffStatus::Active);
    let gender: Option<Gender> = gender.as_deref().and_then(Gender::parse);
    Ok(StaffCreate {
        subject_ref: None,
        given_name: row.get("given_name").unwrap_or("").to_owned(),
        family_name: row.get("family_name").map(str::to_owned),
        display_name: row.get("display_name").map(str::to_owned),
        work_email: row.get("work_email").map(str::to_owned),
        phone: row.get("phone").map(str::to_owned),
        date_of_birth,
        gender,
        staff_category,
        initial_status: Some(initial_status),
    })
}

struct StaffWriter {
    scope: ExecutionScope,
    institution: InstitutionId,
    actor: Actor,
}

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for StaffWriter {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let body = staff_body(row)?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM people.staff_member WHERE tenant_id = $1 \
             AND lower(work_email) = lower($2) AND status NOT IN ('merged', 'archived') LIMIT 1",
        )
        .bind(self.scope.tenant_id.as_uuid())
        .bind(body.work_email.as_deref().unwrap_or(""))
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(id) = existing {
            return Ok(RowOutcome::done(row.line, RowAction::Skip, id));
        }
        let s = create_in(conn, &self.scope, self.institution, body, &self.actor)
            .await
            .map_err(|e| match e {
                KernelError::Validation(v) => KernelError::Validation(
                    v.into_iter()
                        .map(|mut f| {
                            f.field = column_of(&f.field).to_owned();
                            f
                        })
                        .collect(),
                ),
                other => other,
            })?;
        Ok(RowOutcome::done(row.line, RowAction::Create, s.id.as_uuid()))
    }
}

fn staff_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::STAFF,
        label: "Staff",
        owner: "people",
        columns: vec![
            ColumnSpec::new("given_name", "Given name", Text)
                .required()
                .example("Meera"),
            ColumnSpec::new("family_name", "Family name", Text).example("Shah"),
            ColumnSpec::new("display_name", "Display name", Text)
                .help("`given family` when empty"),
            ColumnSpec::new("work_email", "Work e-mail", Email)
                .required()
                .example("meera.shah@college.edu")
                .help("Identifies the record: a live record with this e-mail is skipped"),
            ColumnSpec::new("phone", "Phone", Phone).example("+91 98200 00002"),
            ColumnSpec::new("date_of_birth", "Date of birth", Date).example("1985-02-20"),
            ColumnSpec::new("gender", "Gender", Choice)
                .choices(GENDERS)
                .example("female"),
            ColumnSpec::new("staff_category", "Staff category", Choice)
                .required()
                .choices(CATEGORIES)
                .example("teaching"),
            ColumnSpec::new("status", "Initial status", Choice)
                .choices(STATUSES)
                .example("active")
                .help("`active` when empty (records verified in the legacy system)"),
        ],
        natural_key: &["work_email"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::StaffManage,
    }
}

fn put(m: &mut BTreeMap<String, String>, k: &str, v: Option<String>) {
    if let Some(v) = v.filter(|v| !v.is_empty()) {
        m.insert(k.to_owned(), v);
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgStaffImport {
    fn spec(&self) -> EntitySpec {
        staff_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        if batch.on_existing == OnExisting::Update {
            return Err(KernelError::BadRequest(
                "staff records are maintained through the profile API; use on_existing = skip"
                    .into(),
            ));
        }
        let inst = batch.institution_id;
        let scope = ExecutionScope::institution(scope.tenant_id, inst);
        require_institution_writable(&self.state, &scope, inst).await?;
        let mut w = StaffWriter {
            scope,
            institution: inst,
            actor: Actor {
                id: batch.actor.clone(),
                correlation_id: sutra_web::request_id::current_correlation_id(),
            },
        };
        let run = batch::run(&self.state.pool, &batch.rows, batch.dry_run, &mut w).await?;
        let lines: Vec<u32> = batch.rows.iter().map(|r| r.line).collect();
        Ok(BatchOutcome {
            rows: outcomes(&lines, run.results)?,
            committed: run.committed,
        })
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        let rows = sqlx::query(
            "SELECT given_name, family_name, display_name, work_email, phone, date_of_birth, \
             gender, staff_category, status FROM people.staff_member \
             WHERE tenant_id = $1 AND institution_id = $2 AND status NOT IN ('merged', 'archived') \
             ORDER BY staff_number",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_all(&self.state.pool)
        .await
        .map_err(map_db_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut m = BTreeMap::new();
            for c in [
                "given_name",
                "family_name",
                "display_name",
                "work_email",
                "phone",
                "gender",
                "staff_category",
            ] {
                put(&mut m, c, r.try_get(c).map_err(map_db_error)?);
            }
            let dob: Option<NaiveDate> = r.try_get("date_of_birth").map_err(map_db_error)?;
            put(&mut m, "date_of_birth", dob.map(|d| d.to_string()));
            let status: String = r.try_get("status").map_err(map_db_error)?;
            // Only the creation statuses round-trip; later lifecycle states export as active.
            let status = if STATUSES.contains(&status.as_str()) {
                status
            } else {
                "active".to_owned()
            };
            put(&mut m, "status", Some(status));
            out.push(m);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[(&str, &str)]) -> ImportRow {
        ImportRow {
            line: 2,
            values: cells
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn staff_rows_map_to_the_api_body() {
        let b = staff_body(&row(&[
            ("given_name", "Meera"),
            ("work_email", "meera@college.edu"),
            ("staff_category", "Teaching"),
            ("gender", "female"),
        ]))
        .unwrap();
        assert_eq!(b.staff_category, StaffCategory::Teaching);
        assert_eq!(b.initial_status, Some(StaffStatus::Active));
        assert_eq!(b.gender, Some(Gender::Female));
        let Err(KernelError::Validation(v)) =
            staff_body(&row(&[("given_name", "X"), ("staff_category", "chef")]))
        else {
            panic!("expected validation errors");
        };
        let fields: Vec<&str> = v.iter().map(|f| f.field.as_str()).collect();
        assert!(fields.contains(&"staff_category") && fields.contains(&"work_email"));
        assert_eq!(column_of("workEmail"), "work_email");
    }
}
