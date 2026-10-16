//! Part 2 · S4-import: Domain B import targets (`docs/readiness/part2/s4-import.md`).
//!
//! Each target runs every row through the module's own writer inside the batch transaction
//! ([`sutra_persistence::batch::run`]): the institution guard, the domain validators, reference
//! checks, revision / history rows, events and audit are exactly those of the API.
//!
//! | Entity | Writer | Natural key | Lands in |
//! |--------|--------|-------------|----------|
//! | `campuses` | [`campus::repo::register_in`] | `code` | `registered` |
//! | `org_units` | [`org::unit::register_in`] (parent by `parent_code`, needs a published type blueprint) | `code` | `draft` |
//! | `programmes` | [`curriculum::programme::register_in`] (org unit / campuses by code) | `code` | `draft` |
//! | `curricula` | [`curriculum::framework::create_in`] — one row per course slot, grouped by `curriculum_code` | `curriculum_code` + `slot_code` | `draft` version |
//!
//! Activation / publication stay governed human steps (approvals), so imported structure is
//! reviewed and published in the normal screens before students are migrated into it.

use crate::campus::domain::{CampusAddress, CampusType, RegisterCampus};
use crate::curriculum::framework::{
    ComponentCategory, CreateCurriculum, CreditFramework, CurriculumComponent,
    CurriculumVersionContent, TermStructure,
};
use crate::curriculum::programme::RegisterProgramme;
use crate::org::domain::RegisterOrgUnit;
use crate::shared::Actor;
use crate::{campus, curriculum, org};
use chrono::NaiveDate;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::BTreeMap;
use sutra_contracts::institution::ids::{CampusId, OrgUnitId};
use sutra_contracts::platform::dataimport::{
    entity, outcomes, parse_bool, parse_date, parse_int, row_errors, BatchOutcome, ColumnKind,
    ColumnSpec, EntitySpec, ImportBatch, ImportRow, ImportTargetPort, OnExisting, RowAction,
    RowOutcome,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::batch::{self, ItemWriter};
use sutra_persistence::map_db_error;
use uuid::Uuid;

fn actor_of(batch: &ImportBatch) -> Actor {
    Actor {
        id: batch.actor.clone(),
        correlation_id: sutra_web::request_id::current_correlation_id(),
    }
}

fn violation(field: &str, code: &str, message: impl Into<String>) -> FieldViolation {
    FieldViolation::new(field, code, message.into())
}

fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![violation(field, code, message)])
}

fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

fn opt(row: &ImportRow, c: &str) -> Option<String> {
    row.get(c).map(str::to_owned)
}

fn put(m: &mut BTreeMap<String, String>, k: &str, v: Option<String>) {
    if let Some(v) = v.filter(|v| !v.is_empty()) {
        m.insert(k.to_owned(), v);
    }
}

/// The domain writers name API members (camelCase, nested paths); the import reports columns.
fn to_columns(e: KernelError, names: &[(&str, &str)]) -> KernelError {
    match e {
        KernelError::Validation(v) => KernelError::Validation(
            v.into_iter()
                .map(|mut f| {
                    if let Some((_, c)) = names.iter().find(|(api, _)| f.field == *api) {
                        f.field = (*c).to_owned();
                    }
                    f
                })
                .collect(),
        ),
        other => other,
    }
}

fn reject_update(batch: &ImportBatch, what: &str) -> Result<(), KernelError> {
    if batch.on_existing == OnExisting::Update {
        return Err(KernelError::BadRequest(format!(
            "{what} are changed through their governed modification flow; use on_existing = skip"
        )));
    }
    Ok(())
}

async fn id_by_code(
    conn: &mut PgConnection,
    table: &str,
    scope: &ExecutionScope,
    institution: InstitutionId,
    code: &str,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(&format!(
        "SELECT id FROM institution.{table} WHERE tenant_id = $1 AND institution_id = $2 \
         AND code = $3 ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(institution.as_uuid())
    .bind(code.trim())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Shared batch runner of the row-per-record targets.
async fn run_rows<W: ItemWriter<ImportRow, Output = RowOutcome>>(
    pool: &PgPool,
    batch: &ImportBatch,
    writer: &mut W,
) -> Result<BatchOutcome, KernelError> {
    let run = batch::run(pool, &batch.rows, batch.dry_run, writer).await?;
    let lines: Vec<u32> = batch.rows.iter().map(|r| r.line).collect();
    Ok(BatchOutcome {
        rows: outcomes(&lines, run.results)?,
        committed: run.committed,
    })
}

async fn export_rows(
    pool: &PgPool,
    sql: &str,
    scope: &ExecutionScope,
    institution: InstitutionId,
    columns: &[&str],
) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
    let rows = sqlx::query(sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution.as_uuid())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let mut m = BTreeMap::new();
        for c in columns {
            put(
                &mut m,
                c,
                r.try_get::<Option<String>, _>(*c).map_err(map_db_error)?,
            );
        }
        out.push(m);
    }
    Ok(out)
}

struct Writer {
    scope: ExecutionScope,
    institution: InstitutionId,
    actor: Actor,
}

// ================================================================== campuses

/// `campuses` import target.
#[derive(Clone)]
pub struct PgCampusImport {
    pool: PgPool,
}

impl PgCampusImport {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const CAMPUS_TYPES: &[&str] = &[
    "main",
    "satellite",
    "city",
    "medical",
    "engineering",
    "research",
    "off_campus",
    "online",
    "other",
];

fn campus_cmd(row: &ImportRow) -> Result<RegisterCampus, KernelError> {
    let mut v = Vec::new();
    let campus_type = row.get("campus_type").unwrap_or("satellite").to_lowercase();
    let campus_type = CampusType::parse(&campus_type);
    if campus_type.is_none() {
        v.push(violation(
            "campus_type",
            "choice",
            format!("campus_type must be one of {}", CAMPUS_TYPES.join(", ")),
        ));
    }
    let is_main = match row.get("is_main").map(|b| parse_bool("is_main", b)) {
        Some(Ok(b)) => b,
        Some(Err(e)) => {
            v.push(e);
            false
        }
        None => campus_type == Some(CampusType::Main),
    };
    let established_on = match row
        .get("established_on")
        .map(|d| parse_date("established_on", d))
    {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => None,
    };
    let declared_capacity = match row
        .get("declared_capacity")
        .map(|d| parse_int("declared_capacity", d))
    {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => None,
    };
    finish(v)?;
    let cmd = RegisterCampus {
        code: row.get("code").unwrap_or("").to_uppercase(),
        name: row.get("name").unwrap_or("").to_owned(),
        campus_type: campus_type.unwrap_or(CampusType::Other),
        is_main,
        address: CampusAddress {
            line1: opt(row, "address_line1"),
            line2: opt(row, "address_line2"),
            city: opt(row, "city"),
            state: opt(row, "state"),
            postal_code: opt(row, "postal_code"),
            country: opt(row, "country"),
        },
        geo: None,
        timezone: opt(row, "timezone"),
        established_on,
        declared_capacity,
        contact_email: opt(row, "contact_email"),
        contact_phone: opt(row, "contact_phone"),
        description: opt(row, "description"),
        working_hours: None,
    };
    cmd.validate().map_err(campus_columns)?;
    Ok(cmd)
}

fn campus_columns(e: KernelError) -> KernelError {
    to_columns(
        e,
        &[
            ("isMain", "is_main"),
            ("declaredCapacity", "declared_capacity"),
            ("contactEmail", "contact_email"),
            ("contactPhone", "contact_phone"),
            ("address.line1", "address_line1"),
            ("address.line2", "address_line2"),
            ("address.city", "city"),
            ("address.state", "state"),
            ("address.postalCode", "postal_code"),
            ("address.country", "country"),
        ],
    )
}

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for Writer {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let cmd = campus_cmd(row)?;
        if let Some(id) =
            id_by_code(conn, "campus", &self.scope, self.institution, &cmd.code).await?
        {
            return Ok(RowOutcome::done(row.line, RowAction::Skip, id));
        }
        let c = campus::repo::register_in(conn, &self.scope, self.institution, cmd, &self.actor)
            .await
            .map_err(campus_columns)?;
        Ok(RowOutcome::done(
            row.line,
            RowAction::Create,
            c.id.as_uuid(),
        ))
    }
}

fn campuses_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::CAMPUSES,
        label: "Campuses",
        owner: "institution",
        columns: vec![
            ColumnSpec::new("code", "Code", Code)
                .required()
                .example("MAIN"),
            ColumnSpec::new("name", "Name", Text)
                .required()
                .example("Main Campus"),
            ColumnSpec::new("campus_type", "Campus type", Choice)
                .choices(CAMPUS_TYPES)
                .example("main"),
            ColumnSpec::new("is_main", "Main campus", Boolean).example("true"),
            ColumnSpec::new("address_line1", "Address line 1", Text),
            ColumnSpec::new("address_line2", "Address line 2", Text),
            ColumnSpec::new("city", "City", Text).example("Pune"),
            ColumnSpec::new("state", "State", Text).example("Maharashtra"),
            ColumnSpec::new("postal_code", "Postal code", Text).example("411001"),
            ColumnSpec::new("country", "Country", Text).example("IN"),
            ColumnSpec::new("timezone", "Timezone", Text).example("Asia/Kolkata"),
            ColumnSpec::new("established_on", "Established on", Date).example("1998-06-01"),
            ColumnSpec::new("declared_capacity", "Declared capacity", Integer).example("3000"),
            ColumnSpec::new("contact_email", "Contact e-mail", Email),
            ColumnSpec::new("contact_phone", "Contact phone", Phone),
            ColumnSpec::new("description", "Description", Text),
        ],
        natural_key: &["code"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::CampusManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgCampusImport {
    fn spec(&self) -> EntitySpec {
        campuses_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        reject_update(&batch, "campuses")?;
        let inst = batch.institution_id;
        let mut w = Writer {
            scope: ExecutionScope::institution(scope.tenant_id, inst),
            institution: inst,
            actor: actor_of(&batch),
        };
        run_rows(&self.pool, &batch, &mut w).await
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        export_rows(
            &self.pool,
            "SELECT code, name, campus_type, CASE WHEN is_main THEN 'true' ELSE 'false' END \
             AS is_main, address_line1, address_line2, city, state, postal_code, country, \
             timezone, established_on::text AS established_on, \
             declared_capacity::text AS declared_capacity, contact_email, contact_phone, \
             description FROM institution.campus WHERE tenant_id = $1 AND institution_id = $2 \
             ORDER BY code",
            scope,
            institution_id,
            &[
                "code",
                "name",
                "campus_type",
                "is_main",
                "address_line1",
                "address_line2",
                "city",
                "state",
                "postal_code",
                "country",
                "timezone",
                "established_on",
                "declared_capacity",
                "contact_email",
                "contact_phone",
                "description",
            ],
        )
        .await
    }
}

// ================================================================== organisation units

/// `org_units` import target.
#[derive(Clone)]
pub struct PgOrgUnitImport {
    pool: PgPool,
}

impl PgOrgUnitImport {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

struct OrgUnitWriter(Writer);

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for OrgUnitWriter {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let w = &self.0;
        let code = row.get("code").unwrap_or("").to_uppercase();
        if let Some(id) = id_by_code(conn, "org_unit", &w.scope, w.institution, &code).await? {
            return Ok(RowOutcome::done(row.line, RowAction::Skip, id));
        }
        // Parents first: a parent created by an earlier row of this batch is visible here.
        let parent_id = match row.get("parent_code") {
            None => None,
            Some(p) => Some(OrgUnitId(
                id_by_code(conn, "org_unit", &w.scope, w.institution, &p.to_uppercase())
                    .await?
                    .ok_or_else(|| {
                        invalid(
                            "parent_code",
                            "unknown",
                            format!("unit {p} does not exist (list parents before children)"),
                        )
                    })?,
            )),
        };
        let cmd = RegisterOrgUnit {
            parent_id,
            unit_type: row.get("unit_type").unwrap_or("").to_lowercase(),
            code,
            name: row.get("name").unwrap_or("").to_owned(),
            description: opt(row, "description"),
        };
        let names = [("parentId", "parent_code"), ("unitType", "unit_type")];
        cmd.validate().map_err(|e| to_columns(e, &names))?;
        let u = org::unit::register_in(conn, &w.scope, w.institution, &cmd, &w.actor)
            .await
            .map_err(|e| to_columns(e, &names))?;
        Ok(RowOutcome::done(
            row.line,
            RowAction::Create,
            u.id.as_uuid(),
        ))
    }
}

fn org_units_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::ORG_UNITS,
        label: "Organisation units",
        owner: "institution",
        columns: vec![
            ColumnSpec::new("code", "Code", Code)
                .required()
                .example("CSE"),
            ColumnSpec::new("name", "Name", Text)
                .required()
                .example("Department of Computer Science"),
            ColumnSpec::new("unit_type", "Unit type", Text)
                .required()
                .example("department")
                .help("A unit type key of the published organisation type blueprint"),
            ColumnSpec::new("parent_code", "Parent unit code", Code)
                .example("ENGG")
                .help("Empty for the root unit; parents must come before their children"),
            ColumnSpec::new("description", "Description", Text),
        ],
        natural_key: &["code"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::OrganisationManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgOrgUnitImport {
    fn spec(&self) -> EntitySpec {
        org_units_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        reject_update(&batch, "organisation units")?;
        let inst = batch.institution_id;
        let mut w = OrgUnitWriter(Writer {
            scope: ExecutionScope::institution(scope.tenant_id, inst),
            institution: inst,
            actor: actor_of(&batch),
        });
        run_rows(&self.pool, &batch, &mut w).await
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        export_rows(
            &self.pool,
            "SELECT u.code, u.name, u.unit_type, p.code AS parent_code, u.description \
             FROM institution.org_unit u LEFT JOIN institution.org_unit p ON p.id = u.parent_id \
             WHERE u.tenant_id = $1 AND u.institution_id = $2 ORDER BY u.path",
            scope,
            institution_id,
            &["code", "name", "unit_type", "parent_code", "description"],
        )
        .await
    }
}

// ================================================================== programmes

/// `programmes` import target.
#[derive(Clone)]
pub struct PgProgrammeImport {
    pool: PgPool,
}

impl PgProgrammeImport {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

struct ProgrammeWriter(Writer);

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for ProgrammeWriter {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let w = &self.0;
        let code = row.get("code").unwrap_or("").to_uppercase();
        if let Some(id) = id_by_code(conn, "programme", &w.scope, w.institution, &code).await? {
            return Ok(RowOutcome::done(row.line, RowAction::Skip, id));
        }
        let mut v = Vec::new();
        let duration = match row
            .get("duration_terms")
            .map(|d| parse_int("duration_terms", d))
        {
            Some(Ok(d)) => d,
            Some(Err(e)) => {
                v.push(e);
                0
            }
            None => {
                v.push(violation(
                    "duration_terms",
                    "required",
                    "duration_terms is required",
                ));
                0
            }
        };
        let org_unit_id = match row.get("org_unit_code") {
            None => None,
            Some(c) => {
                match id_by_code(conn, "org_unit", &w.scope, w.institution, &c.to_uppercase())
                    .await?
                {
                    Some(id) => Some(OrgUnitId(id)),
                    None => {
                        v.push(violation(
                            "org_unit_code",
                            "unknown",
                            format!("unit {c} does not exist"),
                        ));
                        None
                    }
                }
            }
        };
        let mut campus_ids = Vec::new();
        for c in row
            .get("campus_codes")
            .unwrap_or("")
            .split([';', ','])
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            match id_by_code(conn, "campus", &w.scope, w.institution, &c.to_uppercase()).await? {
                Some(id) => campus_ids.push(CampusId(id)),
                None => v.push(violation(
                    "campus_codes",
                    "unknown",
                    format!("campus {c} does not exist"),
                )),
            }
        }
        finish(v)?;
        let cmd = RegisterProgramme {
            code,
            name: row.get("name").unwrap_or("").to_owned(),
            level: row.get("level").unwrap_or("").to_lowercase(),
            degree_type: opt(row, "degree_type"),
            duration_terms: duration,
            org_unit_id,
            campus_ids,
        };
        let names = [
            ("durationTerms", "duration_terms"),
            ("degreeType", "degree_type"),
            ("orgUnitId", "org_unit_code"),
            ("campusIds", "campus_codes"),
        ];
        let p = curriculum::programme::register_in(conn, &w.scope, w.institution, &cmd, &w.actor)
            .await
            .map_err(|e| to_columns(e, &names))?;
        Ok(RowOutcome::done(
            row.line,
            RowAction::Create,
            p.id.as_uuid(),
        ))
    }
}

fn programmes_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::PROGRAMMES,
        label: "Programmes",
        owner: "institution",
        columns: vec![
            ColumnSpec::new("code", "Code", Code)
                .required()
                .example("BTECH-CSE"),
            ColumnSpec::new("name", "Name", Text)
                .required()
                .example("B.Tech Computer Science and Engineering"),
            ColumnSpec::new("level", "Level", Text)
                .required()
                .example("ug")
                .help("An academic structure key such as `ug` or `pg`"),
            ColumnSpec::new("degree_type", "Degree", Text).example("B.Tech"),
            ColumnSpec::new("duration_terms", "Duration (terms)", Integer)
                .required()
                .example("8"),
            ColumnSpec::new("org_unit_code", "Organisation unit code", Code).example("CSE"),
            ColumnSpec::new("campus_codes", "Campus codes", Text)
                .example("MAIN;CITY")
                .help("Separated by `;`"),
        ],
        natural_key: &["code"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::CurriculumManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgProgrammeImport {
    fn spec(&self) -> EntitySpec {
        programmes_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        reject_update(&batch, "programmes")?;
        let inst = batch.institution_id;
        let mut w = ProgrammeWriter(Writer {
            scope: ExecutionScope::institution(scope.tenant_id, inst),
            institution: inst,
            actor: actor_of(&batch),
        });
        run_rows(&self.pool, &batch, &mut w).await
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        export_rows(
            &self.pool,
            "SELECT p.code, p.name, p.level, p.degree_type, p.duration_terms::text AS \
             duration_terms, u.code AS org_unit_code, \
             (SELECT string_agg(c.code, ';' ORDER BY c.code) FROM institution.campus c \
              WHERE c.id = ANY(p.campus_ids)) AS campus_codes \
             FROM institution.programme p LEFT JOIN institution.org_unit u ON u.id = p.org_unit_id \
             WHERE p.tenant_id = $1 AND p.institution_id = $2 ORDER BY p.code",
            scope,
            institution_id,
            &[
                "code",
                "name",
                "level",
                "degree_type",
                "duration_terms",
                "org_unit_code",
                "campus_codes",
            ],
        )
        .await
    }
}

// ================================================================== curricula

/// `curricula` import target: one row per course slot; the rows of a curriculum (same
/// `curriculum_code`) form one item, created with its first draft version.
#[derive(Clone)]
pub struct PgCurriculumImport {
    pool: PgPool,
}

impl PgCurriculumImport {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const TERM_STRUCTURES: &[&str] = &["semester", "trimester", "annual"];
const CREDIT_FRAMEWORKS: &[&str] = &["nep2020", "cbcs", "custom"];
const CATEGORIES: &[&str] = &[
    "major_core",
    "major_elective",
    "minor",
    "multidisciplinary",
    "ability_enhancement",
    "skill_enhancement",
    "value_added",
    "internship",
    "research_project",
    "open_elective",
];

fn number(row: &ImportRow, c: &str, v: &mut Vec<FieldViolation>) -> Option<f64> {
    let raw = row.get(c)?;
    match raw.parse::<f64>() {
        Ok(n) if n.is_finite() => Some(n),
        _ => {
            v.push(violation(c, "number", format!("{c} must be a number")));
            None
        }
    }
}

fn hours(row: &ImportRow, c: &str, v: &mut Vec<FieldViolation>) -> i32 {
    match row.get(c).map(|h| parse_int(c, h)) {
        Some(Ok(h)) => h,
        Some(Err(e)) => {
            v.push(e);
            0
        }
        None => 0,
    }
}

fn component(row: &ImportRow) -> Result<CurriculumComponent, KernelError> {
    let mut v = Vec::new();
    let term_no = match row.get("term_no").map(|t| parse_int("term_no", t)) {
        Some(Ok(t)) => t,
        Some(Err(e)) => {
            v.push(e);
            0
        }
        None => {
            v.push(violation("term_no", "required", "term_no is required"));
            0
        }
    };
    let category = row.get("category").unwrap_or("").to_lowercase();
    let category = ComponentCategory::parse(&category);
    if category.is_none() {
        v.push(violation(
            "category",
            "choice",
            format!("category must be one of {}", CATEGORIES.join(", ")),
        ));
    }
    let credits = number(row, "credits", &mut v);
    if row.get("credits").is_none() {
        v.push(violation("credits", "required", "credits is required"));
    }
    let lecture_hours = hours(row, "lecture_hours", &mut v);
    let tutorial_hours = hours(row, "tutorial_hours", &mut v);
    let practical_hours = hours(row, "practical_hours", &mut v);
    finish(v)?;
    Ok(CurriculumComponent {
        term_no,
        slot_code: row.get("slot_code").unwrap_or("").to_uppercase(),
        title: row.get("slot_title").unwrap_or("").to_owned(),
        category: category.unwrap_or(ComponentCategory::MajorCore),
        credits: credits.unwrap_or(0.0),
        lecture_hours,
        tutorial_hours,
        practical_hours,
        basket_code: opt(row, "basket_code"),
        course_code: opt(row, "course_code"),
    })
}

/// Curriculum-level cells, read from the group's first row.
fn content(
    row: &ImportRow,
    components: Vec<CurriculumComponent>,
) -> Result<CreateCurriculum, KernelError> {
    let mut v = Vec::new();
    let effective_from: Option<NaiveDate> = match row
        .get("effective_from")
        .map(|d| parse_date("effective_from", d))
    {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => {
            v.push(violation(
                "effective_from",
                "required",
                "effective_from is required",
            ));
            None
        }
    };
    let ts = row
        .get("term_structure")
        .unwrap_or("semester")
        .to_lowercase();
    let term_structure = TermStructure::parse(&ts);
    if term_structure.is_none() {
        v.push(violation(
            "term_structure",
            "choice",
            format!(
                "term_structure must be one of {}",
                TERM_STRUCTURES.join(", ")
            ),
        ));
    }
    let cf = row
        .get("credit_framework")
        .unwrap_or("nep2020")
        .to_lowercase();
    let credit_framework = CreditFramework::parse(&cf);
    if credit_framework.is_none() {
        v.push(violation(
            "credit_framework",
            "choice",
            format!(
                "credit_framework must be one of {}",
                CREDIT_FRAMEWORKS.join(", ")
            ),
        ));
    }
    let term_count = match row.get("term_count").map(|t| parse_int("term_count", t)) {
        Some(Ok(t)) => t,
        Some(Err(e)) => {
            v.push(e);
            0
        }
        None => components.iter().map(|c| c.term_no).max().unwrap_or(0),
    };
    let min_total_credits = number(row, "min_total_credits", &mut v);
    finish(v)?;
    let (Some(effective_from), Some(term_structure), Some(credit_framework)) =
        (effective_from, term_structure, credit_framework)
    else {
        return Err(invalid(
            "effective_from",
            "required",
            "incomplete curriculum",
        ));
    };
    Ok(CreateCurriculum {
        code: row.get("curriculum_code").unwrap_or("").to_uppercase(),
        title: row.get("curriculum_title").unwrap_or("").to_owned(),
        initial_version: CurriculumVersionContent {
            effective_from,
            applies_from_cohort: opt(row, "applies_from_cohort"),
            change_summary: Some("Migrated from the legacy system".into()),
            term_structure,
            term_count,
            credit_framework,
            min_total_credits,
            min_credits_per_term: None,
            max_credits_per_term: None,
            nep_aligned: credit_framework == CreditFramework::Nep2020,
            nheqf_level: None,
            multiple_entry_exit: false,
            abc_enabled: false,
            exit_awards: vec![],
            components,
        },
    })
}

const CURRICULUM_NAMES: &[(&str, &str)] = &[
    ("code", "curriculum_code"),
    ("title", "curriculum_title"),
    ("initialVersion.effectiveFrom", "effective_from"),
    ("initialVersion.termCount", "term_count"),
    ("initialVersion.minTotalCredits", "min_total_credits"),
];

struct CurriculumWriter(Writer);

#[async_trait::async_trait]
impl ItemWriter<Vec<ImportRow>> for CurriculumWriter {
    type Output = Vec<RowOutcome>;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        rows: &Vec<ImportRow>,
    ) -> Result<Vec<RowOutcome>, KernelError> {
        let w = &self.0;
        let Some(first) = rows.first() else {
            return Ok(vec![]);
        };
        let code = first.get("curriculum_code").unwrap_or("").to_uppercase();
        if let Some(id) = id_by_code(conn, "curriculum", &w.scope, w.institution, &code).await? {
            return Ok(rows
                .iter()
                .map(|r| RowOutcome::done(r.line, RowAction::Skip, id))
                .collect());
        }
        let mut components = Vec::with_capacity(rows.len());
        for r in rows {
            components.push(component(r).map_err(|e| {
                // Name the line inside the group so the error report points at the slot.
                match e {
                    KernelError::Validation(v) => KernelError::Validation(
                        v.into_iter()
                            .map(|mut f| {
                                f.message = format!("line {}: {}", r.line, f.message);
                                f
                            })
                            .collect(),
                    ),
                    other => other,
                }
            })?);
        }
        let mut cmd = content(first, components)?;
        if cmd.title.trim().is_empty() {
            cmd.title = code.clone();
        }
        let programme = first.get("programme_code").unwrap_or("").to_uppercase();
        let Some(pid) = id_by_code(conn, "programme", &w.scope, w.institution, &programme).await?
        else {
            return Err(invalid(
                "programme_code",
                "unknown",
                format!("programme {programme} does not exist in this institution"),
            ));
        };
        let c =
            curriculum::framework::create_in(conn, &w.scope, w.institution, pid, &cmd, &w.actor)
                .await
                .map_err(|e| to_columns(e, CURRICULUM_NAMES))?;
        Ok(rows
            .iter()
            .map(|r| RowOutcome::done(r.line, RowAction::Create, c.id.as_uuid()))
            .collect())
    }
}

fn curricula_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::CURRICULA,
        label: "Curricula and course slots",
        owner: "institution",
        columns: vec![
            ColumnSpec::new("curriculum_code", "Curriculum code", Code)
                .required()
                .example("BTECH-CSE-2023")
                .help("Rows with the same code form one curriculum"),
            ColumnSpec::new("curriculum_title", "Curriculum title", Text)
                .example("B.Tech CSE (2023 scheme)"),
            ColumnSpec::new("programme_code", "Programme code", Code)
                .required()
                .example("BTECH-CSE"),
            ColumnSpec::new("effective_from", "Effective from", Date)
                .required()
                .example("2023-07-01"),
            ColumnSpec::new("applies_from_cohort", "First cohort", Text).example("2023-24"),
            ColumnSpec::new("term_structure", "Term structure", Choice)
                .choices(TERM_STRUCTURES)
                .example("semester"),
            ColumnSpec::new("term_count", "Terms", Integer).example("8"),
            ColumnSpec::new("credit_framework", "Credit framework", Choice)
                .choices(CREDIT_FRAMEWORKS)
                .example("nep2020"),
            ColumnSpec::new("min_total_credits", "Minimum credits", Decimal).example("160"),
            ColumnSpec::new("term_no", "Term", Integer)
                .required()
                .example("1"),
            ColumnSpec::new("slot_code", "Slot code", Code)
                .required()
                .example("CS101"),
            ColumnSpec::new("slot_title", "Slot title", Text)
                .required()
                .example("Programming Fundamentals"),
            ColumnSpec::new("category", "Category", Choice)
                .required()
                .choices(CATEGORIES)
                .example("major_core"),
            ColumnSpec::new("credits", "Credits", Decimal)
                .required()
                .example("4"),
            ColumnSpec::new("lecture_hours", "Lecture hours", Integer).example("3"),
            ColumnSpec::new("tutorial_hours", "Tutorial hours", Integer).example("1"),
            ColumnSpec::new("practical_hours", "Practical hours", Integer).example("0"),
            ColumnSpec::new("basket_code", "Elective basket", Text),
            ColumnSpec::new("course_code", "Course code", Text).example("CS101"),
        ],
        natural_key: &["curriculum_code", "slot_code"],
        amount_column: None,
        group_by: Some("curriculum_code"),
        supports_update: false,
        requires_approval: false,
        permission: Permission::CurriculumManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgCurriculumImport {
    fn spec(&self) -> EntitySpec {
        curricula_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        reject_update(&batch, "curricula")?;
        let inst = batch.institution_id;
        // Group the batch's rows by curriculum (first appearance order).
        let mut order: Vec<String> = Vec::new();
        let mut groups: BTreeMap<String, Vec<ImportRow>> = BTreeMap::new();
        for r in &batch.rows {
            let k = r.get("curriculum_code").unwrap_or("").to_uppercase();
            if !groups.contains_key(&k) {
                order.push(k.clone());
            }
            groups.entry(k).or_default().push(r.clone());
        }
        let items: Vec<Vec<ImportRow>> = order.iter().filter_map(|k| groups.remove(k)).collect();
        let mut w = CurriculumWriter(Writer {
            scope: ExecutionScope::institution(scope.tenant_id, inst),
            institution: inst,
            actor: actor_of(&batch),
        });
        let run = batch::run(&self.pool, &items, batch.dry_run, &mut w).await?;
        let mut rows = Vec::with_capacity(batch.rows.len());
        for (group, result) in items.iter().zip(run.results) {
            match result {
                Ok(o) => rows.extend(o),
                Err(e) => {
                    let Some(errors) = row_errors(&e) else {
                        return Err(e);
                    };
                    rows.extend(
                        group
                            .iter()
                            .map(|r| RowOutcome::error(r.line, errors.clone())),
                    );
                }
            }
        }
        rows.sort_by_key(|o| o.line);
        Ok(BatchOutcome {
            rows,
            committed: run.committed,
        })
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        // Latest version of each curriculum, one row per slot.
        export_rows(
            &self.pool,
            "SELECT c.code AS curriculum_code, c.title AS curriculum_title, \
             p.code AS programme_code, v.effective_from::text AS effective_from, \
             v.applies_from_cohort, v.term_structure, v.term_count::text AS term_count, \
             v.credit_framework, v.min_total_credits::text AS min_total_credits, \
             k.term_no::text AS term_no, k.slot_code, k.title AS slot_title, k.category, \
             k.credits::text AS credits, k.lecture_hours::text AS lecture_hours, \
             k.tutorial_hours::text AS tutorial_hours, k.practical_hours::text AS \
             practical_hours, k.basket_code, k.course_code \
             FROM institution.curriculum c \
             JOIN institution.programme p ON p.id = c.programme_id \
             JOIN LATERAL (SELECT * FROM institution.curriculum_version x \
               WHERE x.curriculum_id = c.id ORDER BY x.version_no DESC LIMIT 1) v ON true \
             JOIN institution.curriculum_component k ON k.curriculum_version_id = v.id \
             WHERE c.tenant_id = $1 AND c.institution_id = $2 \
             ORDER BY c.code, k.term_no, k.slot_code",
            scope,
            institution_id,
            &[
                "curriculum_code",
                "curriculum_title",
                "programme_code",
                "effective_from",
                "applies_from_cohort",
                "term_structure",
                "term_count",
                "credit_framework",
                "min_total_credits",
                "term_no",
                "slot_code",
                "slot_title",
                "category",
                "credits",
                "lecture_hours",
                "tutorial_hours",
                "practical_hours",
                "basket_code",
                "course_code",
            ],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(line: u32, cells: &[(&str, &str)]) -> ImportRow {
        ImportRow {
            line,
            values: cells
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn campus_rows_use_the_domain_validator_and_report_columns() {
        let ok = campus_cmd(&row(
            2,
            &[("code", "main"), ("name", "Main"), ("campus_type", "main")],
        ))
        .unwrap();
        assert!(ok.is_main && ok.code == "MAIN");
        let Err(KernelError::Validation(v)) = campus_cmd(&row(
            2,
            &[
                ("code", "M"),
                ("name", "Main"),
                ("campus_type", "satellite"),
                ("contact_email", "nope"),
            ],
        )) else {
            panic!("expected errors");
        };
        assert!(v.iter().any(|f| f.field == "contact_email"), "{v:?}");
    }

    #[test]
    fn curriculum_slots_build_the_first_version() {
        let base = [
            ("curriculum_code", "BT-2023"),
            ("programme_code", "BT"),
            ("effective_from", "2023-07-01"),
            ("term_no", "1"),
            ("slot_code", "cs101"),
            ("slot_title", "Programming"),
            ("category", "major_core"),
            ("credits", "4"),
        ];
        let c = component(&row(2, &base)).unwrap();
        assert_eq!(c.slot_code, "CS101");
        let cmd = content(&row(2, &base), vec![c]).unwrap();
        assert_eq!(cmd.initial_version.term_count, 1);
        assert_eq!(cmd.initial_version.term_structure, TermStructure::Semester);
        let mut bad = base.to_vec();
        bad.retain(|(k, _)| *k != "credits");
        bad.push(("category", "core"));
        assert!(component(&row(3, &bad)).is_err());
        assert_eq!(curricula_spec().group_by, Some("curriculum_code"));
    }
}
