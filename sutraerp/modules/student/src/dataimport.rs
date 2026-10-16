//! Part 2 · S4-import: Domain C import targets (`docs/readiness/part2/s4-import.md`).
//!
//! * [`PgStudentImport`] (`students`) — a migrated student: persona + `active` StudentContext in
//!   the named programme (current published curriculum version), optional campus, enrolment and
//!   roll numbers, plus an optional guardian (registered + relationship `proposed`, verification
//!   stays a human step). Natural key: the legacy `student_number` (kept as the tenant-unique
//!   student number, so other files can refer to it).
//! * [`PgHistoricalResultImport`] (`historical_results`) — past course results from the legacy
//!   system (append-only `student.historical_result`); a passed result with credits posts an
//!   `earned` entry to the credit ledger (same writer and `credit_ledger.updated` event the
//!   progression code uses). Natural key: student number + course + term + attempt.
//!
//! Both run every row through [`sutra_persistence::batch::run`]: one transaction per batch, one
//! savepoint per row, rolled back for a dry run or when any row fails.

use crate::guardian::domain::{NewGuardian, RelationshipType};
use crate::progression::repo::{append_entry, ledger_updated, NewEntry};
use crate::record::creation::allocate_enrolment_number;
use crate::record::support::{
    finish, status_changed, validate_email, validate_opt_text, validate_phone, validate_text,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::publish;
use crate::shared::student::{
    context_from_row, student_from_row, StudentContextRow, CONTEXT_COLUMNS, STUDENT_COLUMNS,
};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::institution::status::PublicationStatus;
use sutra_contracts::platform::dataimport::{
    entity, outcomes, parse_amount, parse_date, parse_int, BatchOutcome, ColumnKind, ColumnSpec,
    EntitySpec, ImportBatch, ImportRow, ImportTargetPort, OnExisting, RowAction, RowOutcome,
};
use sutra_contracts::student::events::record::{
    StudentCreatedV1, STUDENT_CREATED, STUDENT_CREATED_SCHEMA, STUDENT_ENROLLED,
    STUDENT_ENROLLED_SCHEMA,
};
use sutra_contracts::student::status::CreditEntryKind;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::batch::{self, ItemWriter};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Source type of migrated credit-ledger entries and student records.
pub const MIGRATION_SOURCE: &str = "data_migration";

const RELATIONSHIPS: &[&str] = &[
    "mother",
    "father",
    "legal_guardian",
    "grandparent",
    "sibling",
    "relative",
    "sponsor",
    "other",
];
const ENROLMENT_TYPES: &[&str] = &["regular", "lateral_entry", "transfer_in", "re_entry"];
const RESULT_STATUSES: &[&str] = &["pass", "fail", "absent", "withheld", "exempted"];

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

/// Student numbers keep the legacy format the database accepts.
fn student_number_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'_' | b'.' | b'-'))
}

async fn student_id_by_number(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    number: &str,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM student.student WHERE tenant_id = $1 AND student_number = $2",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(number)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

fn opt(row: &ImportRow, c: &str) -> Option<String> {
    row.get(c).map(str::to_owned)
}

fn date_cell(row: &ImportRow, c: &str, v: &mut Vec<FieldViolation>) -> Option<NaiveDate> {
    match row.get(c).map(|d| parse_date(c, d)) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => None,
    }
}

fn int_cell(row: &ImportRow, c: &str, max: i32, v: &mut Vec<FieldViolation>) -> i32 {
    match row.get(c).map(|t| parse_int(c, t)) {
        Some(Ok(t)) if (1..=max).contains(&t) => t,
        Some(Ok(_)) => {
            v.push(violation(
                c,
                "range",
                format!("{c} must be between 1 and {max}"),
            ));
            1
        }
        Some(Err(e)) => {
            v.push(e);
            1
        }
        None => 1,
    }
}

fn decimal_cell(row: &ImportRow, c: &str, v: &mut Vec<FieldViolation>) -> Option<Decimal> {
    match row.get(c).map(|x| parse_amount(c, x)) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => None,
    }
}

// ================================================================== students

/// `students` import target.
#[derive(Clone)]
pub struct PgStudentImport {
    state: StudentState,
}

impl PgStudentImport {
    pub fn new(state: StudentState) -> Self {
        Self { state }
    }
}

/// Validated cells of one student row.
struct StudentCells {
    line: u32,
    student_number: String,
    given_name: String,
    middle_name: Option<String>,
    family_name: Option<String>,
    date_of_birth: Option<NaiveDate>,
    gender: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    programme_code: String,
    campus_code: Option<String>,
    enrolment_number: Option<String>,
    roll_number: Option<String>,
    enrolment_type: String,
    enrolled_on: Option<NaiveDate>,
    current_term_no: i32,
    batch_code: Option<String>,
    section_code: Option<String>,
    guardian: Option<GuardianCells>,
}

struct GuardianCells {
    given_name: String,
    family_name: Option<String>,
    relationship: RelationshipType,
    phone: Option<String>,
    email: Option<String>,
}

fn guardian_cells(row: &ImportRow, v: &mut Vec<FieldViolation>) -> Option<GuardianCells> {
    let Some(name) = row.get("guardian_name") else {
        if ["guardian_relationship", "guardian_phone", "guardian_email"]
            .iter()
            .any(|c| row.get(c).is_some())
        {
            v.push(violation(
                "guardian_name",
                "required",
                "guardian_name is required when other guardian columns are filled",
            ));
        }
        return None;
    };
    validate_text("guardian_name", name, 200, v);
    validate_phone("guardian_phone", row.get("guardian_phone"), v);
    validate_email("guardian_email", row.get("guardian_email"), v);
    if row.get("guardian_phone").is_none() && row.get("guardian_email").is_none() {
        v.push(violation(
            "guardian_phone",
            "required",
            "a guardian needs a phone number or an e-mail address",
        ));
    }
    let rel = row
        .get("guardian_relationship")
        .unwrap_or("legal_guardian")
        .to_lowercase();
    let parsed: Option<RelationshipType> =
        serde_json::from_value(serde_json::Value::String(rel)).ok();
    if parsed.is_none() {
        v.push(violation(
            "guardian_relationship",
            "choice",
            format!(
                "guardian_relationship must be one of {}",
                RELATIONSHIPS.join(", ")
            ),
        ));
    }
    let (given, family) = match name.trim().rsplit_once(' ') {
        Some((g, f)) => (g.trim().to_owned(), Some(f.trim().to_owned())),
        None => (name.trim().to_owned(), None),
    };
    parsed.map(|relationship| GuardianCells {
        given_name: given,
        family_name: family,
        relationship,
        phone: opt(row, "guardian_phone"),
        email: opt(row, "guardian_email"),
    })
}

fn student_cells(row: &ImportRow) -> Result<StudentCells, KernelError> {
    let mut v = Vec::new();
    let number = row.get("student_number").unwrap_or("").to_owned();
    if !student_number_ok(&number) {
        v.push(violation(
            "student_number",
            "format",
            "student_number must be 1-40 letters, digits, `/`, `_`, `.` or `-`",
        ));
    }
    let given = row.get("given_name").unwrap_or("").to_owned();
    validate_text("given_name", &given, 100, &mut v);
    validate_opt_text("middle_name", row.get("middle_name"), 100, &mut v);
    validate_opt_text("family_name", row.get("family_name"), 100, &mut v);
    validate_email("email", row.get("email"), &mut v);
    validate_phone("phone", row.get("phone"), &mut v);
    for c in ["enrolment_number", "roll_number", "batch_code", "section_code"] {
        validate_opt_text(c, row.get(c), 40, &mut v);
    }
    let dob = date_cell(row, "date_of_birth", &mut v);
    if dob.is_some_and(|d| d > chrono::Utc::now().date_naive()) {
        v.push(violation(
            "date_of_birth",
            "range",
            "date_of_birth must be in the past",
        ));
    }
    let enrolled_on = date_cell(row, "enrolled_on", &mut v);
    let term = int_cell(row, "current_term_no", 20, &mut v);
    let enrolment_type = row
        .get("enrolment_type")
        .unwrap_or("regular")
        .to_lowercase();
    if !ENROLMENT_TYPES.contains(&enrolment_type.as_str()) {
        v.push(violation(
            "enrolment_type",
            "choice",
            format!(
                "enrolment_type must be one of {}",
                ENROLMENT_TYPES.join(", ")
            ),
        ));
    }
    let programme_code = row.get("programme_code").unwrap_or("").to_owned();
    if programme_code.is_empty() {
        v.push(violation(
            "programme_code",
            "required",
            "programme_code is required",
        ));
    }
    let guardian = guardian_cells(row, &mut v);
    finish(v)?;
    Ok(StudentCells {
        line: row.line,
        student_number: number,
        given_name: given.trim().to_owned(),
        middle_name: opt(row, "middle_name"),
        family_name: opt(row, "family_name"),
        date_of_birth: dob,
        gender: opt(row, "gender").map(|g| g.to_lowercase()),
        email: opt(row, "email"),
        phone: opt(row, "phone"),
        programme_code,
        campus_code: opt(row, "campus_code"),
        enrolment_number: opt(row, "enrolment_number"),
        roll_number: opt(row, "roll_number"),
        enrolment_type,
        enrolled_on,
        current_term_no: term,
        batch_code: opt(row, "batch_code"),
        section_code: opt(row, "section_code"),
        guardian,
    })
}

type Resolved<T> = Result<T, FieldViolation>;

struct StudentWriter<'a> {
    state: &'a StudentState,
    scope: ExecutionScope,
    institution: InstitutionId,
    today: NaiveDate,
    actor: Actor,
    /// Domain B references resolved once per code (programme → id + current curriculum).
    programmes: HashMap<String, Resolved<(Uuid, Option<Uuid>)>>,
    campuses: HashMap<String, Resolved<Uuid>>,
}

impl StudentWriter<'_> {
    async fn programme(&mut self, code: &str) -> Result<(Uuid, Option<Uuid>), KernelError> {
        let key = code.to_uppercase();
        if let Some(r) = self.programmes.get(&key) {
            return r.clone().map_err(|v| KernelError::Validation(vec![v]));
        }
        let found = self
            .state
            .references
            .programme_by_code(&self.scope, self.institution, code)
            .await?;
        let r = match found {
            None => Err(violation(
                "programme_code",
                "unknown",
                format!("programme {code} does not exist in this institution"),
            )),
            Some(p) if p.status != PublicationStatus::Published => Err(violation(
                "programme_code",
                "not_published",
                format!("programme {code} is not published"),
            )),
            Some(p) => {
                let cv = self
                    .state
                    .references
                    .current_curriculum_version(&self.scope, p.id, self.today)
                    .await?;
                Ok((p.id, cv.map(|c| c.id)))
            }
        };
        self.programmes.insert(key, r.clone());
        r.map_err(|v| KernelError::Validation(vec![v]))
    }

    async fn campus(&mut self, code: &str) -> Result<Uuid, KernelError> {
        let key = code.to_uppercase();
        if let Some(r) = self.campuses.get(&key) {
            return r.clone().map_err(|v| KernelError::Validation(vec![v]));
        }
        let found = self
            .state
            .references
            .campus_by_code(&self.scope, self.institution, code)
            .await?;
        let r = match found {
            Some(c) => Ok(c.id),
            None => Err(violation(
                "campus_code",
                "unknown",
                format!("campus {code} does not exist in this institution"),
            )),
        };
        self.campuses.insert(key, r.clone());
        r.map_err(|v| KernelError::Validation(vec![v]))
    }
}

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for StudentWriter<'_> {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let c = student_cells(row)?;
        if let Some(id) = student_id_by_number(conn, &self.scope, &c.student_number).await? {
            return Ok(RowOutcome::done(c.line, RowAction::Skip, id));
        }
        let (programme_id, curriculum_version_id) = self.programme(&c.programme_code).await?;
        let campus_id = match &c.campus_code {
            Some(code) => Some(self.campus(code).await?),
            None => None,
        };
        let tenant = self.scope.tenant_id;
        let inst = self.institution;
        let srow = sqlx::query(&format!(
            "INSERT INTO student.student (tenant_id, student_number, given_name, middle_name, \
             family_name, date_of_birth, gender, primary_email, primary_phone, status) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'active') RETURNING {STUDENT_COLUMNS}"
        ))
        .bind(tenant.as_uuid())
        .bind(&c.student_number)
        .bind(&c.given_name)
        .bind(&c.middle_name)
        .bind(&c.family_name)
        .bind(c.date_of_birth)
        .bind(&c.gender)
        .bind(&c.email)
        .bind(&c.phone)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let student = student_from_row(&srow).map_err(map_db_error)?;
        sqlx::query(
            "INSERT INTO student.communication_preference (student_id, tenant_id, \
             email_opt_in, sms_opt_in) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(student.id.as_uuid())
        .bind(tenant.as_uuid())
        .bind(student.primary_email.is_some())
        .bind(student.primary_phone.is_some())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let enrolment_number = match &c.enrolment_number {
            Some(n) => n.clone(),
            None => {
                allocate_enrolment_number(conn, &self.scope, inst.as_uuid(), self.today).await?
            }
        };
        let crow = sqlx::query(&format!(
            "INSERT INTO student.student_context (tenant_id, student_id, institution_id, \
             campus_id, programme_id, curriculum_version_id, current_term_no, \
             enrolment_number, roll_number, enrolment_type, enrolled_on, batch_code, \
             section_code, status) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'active') \
             RETURNING {CONTEXT_COLUMNS}"
        ))
        .bind(tenant.as_uuid())
        .bind(student.id.as_uuid())
        .bind(inst.as_uuid())
        .bind(campus_id)
        .bind(programme_id)
        .bind(curriculum_version_id)
        .bind(c.current_term_no)
        .bind(&enrolment_number)
        .bind(&c.roll_number)
        .bind(&c.enrolment_type)
        .bind(c.enrolled_on.unwrap_or(self.today))
        .bind(&c.batch_code)
        .bind(&c.section_code)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d)
                if d.constraint() == Some("student_context_enrolment_number_unique") =>
            {
                invalid(
                    "enrolment_number",
                    "duplicate",
                    "enrolment_number is already used in this institution",
                )
            }
            _ => map_db_error(e),
        })?;
        let ctx: StudentContextRow = context_from_row(&crow).map_err(map_db_error)?;
        publish::event(
            conn,
            &self.scope,
            STUDENT_CREATED,
            STUDENT_CREATED_SCHEMA,
            student.id.to_string(),
            &StudentCreatedV1 {
                student_id: student.id.as_uuid(),
                student_context_id: ctx.id.as_uuid(),
                institution_id: inst.as_uuid(),
                admission_id: None,
                student_number: student.student_number.clone(),
                programme_id: Some(programme_id),
                student_subject_ref: None,
            },
            &self.actor,
        )
        .await?;
        publish::event(
            conn,
            &self.scope,
            STUDENT_ENROLLED,
            STUDENT_ENROLLED_SCHEMA,
            ctx.id.to_string(),
            &status_changed(
                ctx.id.as_uuid(),
                inst,
                Some(student.id.as_uuid()),
                None,
                ctx.status.as_str(),
                ctx.version.0,
                Some("migrated from the legacy system".into()),
                None,
                None,
            ),
            &self.actor,
        )
        .await?;
        if let Some(g) = &c.guardian {
            crate::guardian::repo::import_guardian(
                conn,
                &self.scope,
                inst,
                student.id,
                NewGuardian {
                    given_name: g.given_name.clone(),
                    family_name: g.family_name.clone(),
                    email: g.email.clone(),
                    phone: g.phone.clone(),
                    address: None,
                    occupation: None,
                },
                g.relationship,
                true,
                &self.actor,
            )
            .await?;
        }
        publish::audit(
            conn,
            &self.scope,
            "student.student.import",
            "student",
            student.id.to_string(),
            serde_json::json!({
                "studentNumber": student.student_number,
                "studentContextId": ctx.id,
                "programmeId": programme_id,
                "enrolmentNumber": enrolment_number,
                "guardian": c.guardian.is_some(),
                "source": MIGRATION_SOURCE,
            }),
            &self.actor,
        )
        .await?;
        Ok(RowOutcome::done(
            c.line,
            RowAction::Create,
            student.id.as_uuid(),
        ))
    }
}

fn students_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::STUDENTS,
        label: "Students (with a guardian)",
        owner: "student",
        columns: vec![
            ColumnSpec::new("student_number", "Student number", Text)
                .required()
                .example("S2023/0001")
                .help("Legacy student number; kept as the student number (unique in the tenant)"),
            ColumnSpec::new("given_name", "Given name", Text)
                .required()
                .example("Aarav"),
            ColumnSpec::new("middle_name", "Middle name", Text),
            ColumnSpec::new("family_name", "Family name", Text).example("Desai"),
            ColumnSpec::new("date_of_birth", "Date of birth", Date).example("2005-04-12"),
            ColumnSpec::new("gender", "Gender", Text).example("male"),
            ColumnSpec::new("email", "E-mail", Email).example("aarav@example.org"),
            ColumnSpec::new("phone", "Phone", Phone).example("+91 98200 00001"),
            ColumnSpec::new("programme_code", "Programme code", Code)
                .required()
                .example("BTECH-CSE")
                .help("A published programme of the institution"),
            ColumnSpec::new("campus_code", "Campus code", Code).example("MAIN"),
            ColumnSpec::new("enrolment_number", "Enrolment number", Text)
                .example("EN2023-0001")
                .help("Allocated automatically when empty"),
            ColumnSpec::new("roll_number", "Roll number", Text),
            ColumnSpec::new("enrolment_type", "Enrolment type", Choice)
                .choices(ENROLMENT_TYPES)
                .example("regular"),
            ColumnSpec::new("enrolled_on", "Enrolled on", Date).example("2023-08-01"),
            ColumnSpec::new("current_term_no", "Current term", Integer).example("3"),
            ColumnSpec::new("batch_code", "Batch", Text),
            ColumnSpec::new("section_code", "Section", Text),
            ColumnSpec::new("guardian_name", "Guardian name", Text).example("Meena Desai"),
            ColumnSpec::new("guardian_relationship", "Guardian relationship", Choice)
                .choices(RELATIONSHIPS)
                .example("mother"),
            ColumnSpec::new("guardian_phone", "Guardian phone", Phone),
            ColumnSpec::new("guardian_email", "Guardian e-mail", Email),
        ],
        natural_key: &["student_number"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::StudentManage,
    }
}

fn put(m: &mut BTreeMap<String, String>, k: &str, v: Option<String>) {
    if let Some(v) = v.filter(|v| !v.is_empty()) {
        m.insert(k.to_owned(), v);
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgStudentImport {
    fn spec(&self) -> EntitySpec {
        students_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        if batch.on_existing == OnExisting::Update {
            return Err(KernelError::BadRequest(
                "students are never updated by an import; use on_existing = skip".into(),
            ));
        }
        let inst = batch.institution_id;
        let scope = ExecutionScope::institution(scope.tenant_id, inst);
        require_institution_writable(&self.state, &scope, inst).await?;
        let today = business_today(&self.state, &scope, inst).await?;
        let mut w = StudentWriter {
            state: &self.state,
            scope,
            institution: inst,
            today,
            actor: actor_of(&batch),
            programmes: HashMap::new(),
            campuses: HashMap::new(),
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
            "SELECT s.student_number, s.given_name, s.middle_name, s.family_name, \
             s.date_of_birth, s.gender, s.primary_email, s.primary_phone, c.programme_id, \
             c.campus_id, c.enrolment_number, c.roll_number, c.enrolment_type, c.enrolled_on, \
             c.current_term_no, c.batch_code, c.section_code \
             FROM student.student s JOIN LATERAL ( \
               SELECT * FROM student.student_context x WHERE x.student_id = s.id \
               AND x.tenant_id = s.tenant_id AND x.institution_id = $2 \
               ORDER BY x.created_at DESC, x.id DESC LIMIT 1) c ON true \
             WHERE s.tenant_id = $1 ORDER BY s.student_number",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_all(&self.state.pool)
        .await
        .map_err(map_db_error)?;
        let mut programmes: HashMap<Uuid, Option<String>> = HashMap::new();
        let mut campuses: HashMap<Uuid, Option<String>> = HashMap::new();
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut m = BTreeMap::new();
            let get = |c: &str| -> Result<Option<String>, KernelError> {
                r.try_get::<Option<String>, _>(c).map_err(map_db_error)
            };
            put(&mut m, "student_number", get("student_number")?);
            put(&mut m, "given_name", get("given_name")?);
            put(&mut m, "middle_name", get("middle_name")?);
            put(&mut m, "family_name", get("family_name")?);
            let dob: Option<NaiveDate> = r.try_get("date_of_birth").map_err(map_db_error)?;
            put(&mut m, "date_of_birth", dob.map(|d| d.to_string()));
            put(&mut m, "gender", get("gender")?);
            put(&mut m, "email", get("primary_email")?);
            put(&mut m, "phone", get("primary_phone")?);
            let pid: Option<Uuid> = r.try_get("programme_id").map_err(map_db_error)?;
            if let Some(pid) = pid {
                let code = match programmes.get(&pid) {
                    Some(code) => code.clone(),
                    None => {
                        let code = self
                            .state
                            .references
                            .programme(scope, pid)
                            .await?
                            .map(|p| p.code);
                        programmes.insert(pid, code.clone());
                        code
                    }
                };
                put(&mut m, "programme_code", code);
            }
            let cid: Option<Uuid> = r.try_get("campus_id").map_err(map_db_error)?;
            if let Some(cid) = cid {
                let code = match campuses.get(&cid) {
                    Some(code) => code.clone(),
                    None => {
                        let code = self
                            .state
                            .references
                            .campus(scope, cid)
                            .await?
                            .map(|c| c.code);
                        campuses.insert(cid, code.clone());
                        code
                    }
                };
                put(&mut m, "campus_code", code);
            }
            put(&mut m, "enrolment_number", get("enrolment_number")?);
            put(&mut m, "roll_number", get("roll_number")?);
            put(&mut m, "enrolment_type", get("enrolment_type")?);
            let on: Option<NaiveDate> = r.try_get("enrolled_on").map_err(map_db_error)?;
            put(&mut m, "enrolled_on", on.map(|d| d.to_string()));
            let term: Option<i32> = r.try_get("current_term_no").map_err(map_db_error)?;
            put(&mut m, "current_term_no", term.map(|t| t.to_string()));
            put(&mut m, "batch_code", get("batch_code")?);
            put(&mut m, "section_code", get("section_code")?);
            out.push(m);
        }
        Ok(out)
    }
}

// ================================================================== historical results

/// `historical_results` import target.
#[derive(Clone)]
pub struct PgHistoricalResultImport {
    state: StudentState,
}

impl PgHistoricalResultImport {
    pub fn new(state: StudentState) -> Self {
        Self { state }
    }
}

struct ResultCells {
    line: u32,
    student_number: String,
    term_label: String,
    academic_year_label: Option<String>,
    course_code: String,
    course_title: String,
    credits: Decimal,
    grade: Option<String>,
    grade_point: Option<Decimal>,
    marks: Option<Decimal>,
    max_marks: Option<Decimal>,
    result_status: String,
    attempt: i32,
}

fn course_code_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'/' | b'-'))
}

fn result_cells(row: &ImportRow) -> Result<ResultCells, KernelError> {
    let mut v = Vec::new();
    let number = row.get("student_number").unwrap_or("").to_owned();
    if !student_number_ok(&number) {
        v.push(violation(
            "student_number",
            "format",
            "student_number is not a valid student number",
        ));
    }
    let term = row.get("term_label").unwrap_or("").to_owned();
    validate_text("term_label", &term, 40, &mut v);
    validate_opt_text(
        "academic_year_label",
        row.get("academic_year_label"),
        20,
        &mut v,
    );
    let course = row.get("course_code").unwrap_or("").to_owned();
    if !course_code_ok(&course) {
        v.push(violation(
            "course_code",
            "format",
            "course_code must be 1-32 letters, digits, `_`, `.`, `/` or `-`",
        ));
    }
    let title = row.get("course_title").unwrap_or("").to_owned();
    validate_text("course_title", &title, 200, &mut v);
    validate_opt_text("grade", row.get("grade"), 8, &mut v);
    if row.get("credits").is_none() {
        v.push(violation("credits", "required", "credits is required"));
    }
    let credits = decimal_cell(row, "credits", &mut v);
    if credits.is_some_and(|c| c < Decimal::ZERO || c > Decimal::from(100)) {
        v.push(violation(
            "credits",
            "range",
            "credits must be between 0 and 100",
        ));
    }
    let gp = decimal_cell(row, "grade_point", &mut v);
    if gp.is_some_and(|g| g < Decimal::ZERO || g > Decimal::from(10)) {
        v.push(violation(
            "grade_point",
            "range",
            "grade_point must be between 0 and 10",
        ));
    }
    let marks = decimal_cell(row, "marks", &mut v);
    let max = decimal_cell(row, "max_marks", &mut v);
    if marks.is_some_and(|m| m < Decimal::ZERO) {
        v.push(violation("marks", "range", "marks cannot be negative"));
    }
    if max.is_some_and(|m| m <= Decimal::ZERO) {
        v.push(violation("max_marks", "range", "max_marks must be positive"));
    }
    if let (Some(m), Some(x)) = (marks, max) {
        if m > x {
            v.push(violation("marks", "range", "marks cannot exceed max_marks"));
        }
    }
    let status = row.get("result_status").unwrap_or("").to_lowercase();
    if !RESULT_STATUSES.contains(&status.as_str()) {
        v.push(violation(
            "result_status",
            "choice",
            format!(
                "result_status must be one of {}",
                RESULT_STATUSES.join(", ")
            ),
        ));
    }
    let attempt = int_cell(row, "attempt", 20, &mut v);
    finish(v)?;
    Ok(ResultCells {
        line: row.line,
        student_number: number,
        term_label: term.trim().to_owned(),
        academic_year_label: opt(row, "academic_year_label"),
        course_code: course,
        course_title: title.trim().to_owned(),
        credits: credits.unwrap_or_default(),
        grade: opt(row, "grade"),
        grade_point: gp,
        marks,
        max_marks: max,
        result_status: status,
        attempt,
    })
}

struct ResultWriter {
    scope: ExecutionScope,
    institution: InstitutionId,
    job_id: Uuid,
    actor: Actor,
}

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for ResultWriter {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let c = result_cells(row)?;
        let tenant = self.scope.tenant_id;
        let Some(student_id) = student_id_by_number(conn, &self.scope, &c.student_number).await?
        else {
            return Err(invalid(
                "student_number",
                "unknown",
                format!(
                    "student {} does not exist; import students first",
                    c.student_number
                ),
            ));
        };
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM student.historical_result WHERE tenant_id = $1 AND student_id = $2 \
             AND course_code = $3 AND term_label = $4 AND attempt = $5",
        )
        .bind(tenant.as_uuid())
        .bind(student_id)
        .bind(&c.course_code)
        .bind(&c.term_label)
        .bind(c.attempt)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(id) = existing {
            return Ok(RowOutcome::done(c.line, RowAction::Skip, id));
        }
        // Latest context of the student in this institution (live or closed: alumni results
        // are migrated too); locked so credit postings serialise as in progression.
        let ctx_row = sqlx::query(&format!(
            "SELECT {CONTEXT_COLUMNS} FROM student.student_context WHERE tenant_id = $1 \
             AND student_id = $2 AND institution_id = $3 ORDER BY created_at DESC, id DESC \
             LIMIT 1 FOR UPDATE"
        ))
        .bind(tenant.as_uuid())
        .bind(student_id)
        .bind(self.institution.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let Some(ctx_row) = ctx_row else {
            return Err(invalid(
                "student_number",
                "other_institution",
                "the student has no record in this institution",
            ));
        };
        let ctx = context_from_row(&ctx_row).map_err(map_db_error)?;
        let id = Uuid::now_v7();
        let ledger_entry = if c.result_status == "pass" && c.credits > Decimal::ZERO {
            let note = format!("{} {} ({})", c.course_code, c.course_title, c.term_label);
            let entry = append_entry(
                conn,
                &ctx,
                &NewEntry {
                    kind: CreditEntryKind::Earned,
                    credits: c.credits.to_f64().unwrap_or(0.0),
                    component: None,
                    term: None,
                    source_type: MIGRATION_SOURCE,
                    source_id: Some(id),
                    reverses: None,
                    approved: true,
                    note: Some(note.as_str()),
                },
                &self.actor,
            )
            .await?;
            ledger_updated(conn, &self.scope, &ctx, vec![entry], &self.actor).await?;
            Some(entry)
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO student.historical_result (id, tenant_id, institution_id, student_id, \
             student_context_id, term_label, academic_year_label, course_code, course_title, \
             credits, grade, grade_point, marks, max_marks, result_status, attempt, source, \
             import_job_id, credit_ledger_entry_id, recorded_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(self.institution.as_uuid())
        .bind(student_id)
        .bind(ctx.id.as_uuid())
        .bind(&c.term_label)
        .bind(&c.academic_year_label)
        .bind(&c.course_code)
        .bind(&c.course_title)
        .bind(c.credits)
        .bind(&c.grade)
        .bind(c.grade_point)
        .bind(c.marks)
        .bind(c.max_marks)
        .bind(&c.result_status)
        .bind(c.attempt)
        .bind(MIGRATION_SOURCE)
        .bind(self.job_id)
        .bind(ledger_entry)
        .bind(&self.actor.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        publish::audit(
            conn,
            &self.scope,
            "student.historical_result.import",
            "historical_result",
            id.to_string(),
            serde_json::json!({
                "studentId": student_id, "courseCode": c.course_code, "termLabel": c.term_label,
                "resultStatus": c.result_status, "credits": c.credits,
                "creditLedgerEntryId": ledger_entry, "importJobId": self.job_id,
            }),
            &self.actor,
        )
        .await?;
        Ok(RowOutcome::done(c.line, RowAction::Create, id))
    }
}

fn results_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::HISTORICAL_RESULTS,
        label: "Historical results",
        owner: "student",
        columns: vec![
            ColumnSpec::new("student_number", "Student number", Text)
                .required()
                .example("S2023/0001"),
            ColumnSpec::new("term_label", "Term", Text)
                .required()
                .example("2023-24 Sem 1")
                .help("The legacy term label (need not exist in the calendar)"),
            ColumnSpec::new("academic_year_label", "Academic year", Text).example("2023-24"),
            ColumnSpec::new("course_code", "Course code", Text)
                .required()
                .example("CS101"),
            ColumnSpec::new("course_title", "Course title", Text)
                .required()
                .example("Programming Fundamentals"),
            ColumnSpec::new("credits", "Credits", Decimal)
                .required()
                .example("4"),
            ColumnSpec::new("grade", "Grade", Text).example("A"),
            ColumnSpec::new("grade_point", "Grade point", Decimal).example("9"),
            ColumnSpec::new("marks", "Marks", Decimal).example("86"),
            ColumnSpec::new("max_marks", "Maximum marks", Decimal).example("100"),
            ColumnSpec::new("result_status", "Result", Choice)
                .required()
                .choices(RESULT_STATUSES)
                .example("pass"),
            ColumnSpec::new("attempt", "Attempt", Integer)
                .example("1")
                .help("1 when empty"),
        ],
        natural_key: &["student_number", "course_code", "term_label", "attempt"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::StudentManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgHistoricalResultImport {
    fn spec(&self) -> EntitySpec {
        results_spec()
    }

    /// The attempt defaults to 1, so `attempt` may be empty in the key.
    fn natural_key(&self, row: &ImportRow) -> Option<String> {
        let attempt = row.get("attempt").unwrap_or("1").trim_start_matches('0');
        Some(format!(
            "{}|{}|{}|{}",
            row.get("student_number")?.to_lowercase(),
            row.get("course_code")?.to_lowercase(),
            row.get("term_label")?.to_lowercase(),
            attempt,
        ))
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        if batch.on_existing == OnExisting::Update {
            return Err(KernelError::BadRequest(
                "historical results are append-only; use on_existing = skip".into(),
            ));
        }
        let inst = batch.institution_id;
        let scope = ExecutionScope::institution(scope.tenant_id, inst);
        require_institution_writable(&self.state, &scope, inst).await?;
        let mut w = ResultWriter {
            scope,
            institution: inst,
            job_id: batch.job_id,
            actor: actor_of(&batch),
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
            "SELECT s.student_number, h.term_label, h.academic_year_label, h.course_code, \
             h.course_title, h.credits, h.grade, h.grade_point, h.marks, h.max_marks, \
             h.result_status, h.attempt FROM student.historical_result h \
             JOIN student.student s ON s.id = h.student_id AND s.tenant_id = h.tenant_id \
             WHERE h.tenant_id = $1 AND h.institution_id = $2 \
             ORDER BY s.student_number, h.term_label, h.course_code, h.attempt",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_all(&self.state.pool)
        .await
        .map_err(map_db_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut m = BTreeMap::new();
            let get = |c: &str| -> Result<Option<String>, KernelError> {
                r.try_get::<Option<String>, _>(c).map_err(map_db_error)
            };
            let dec = |c: &str| -> Result<Option<String>, KernelError> {
                let v: Option<Decimal> = r.try_get(c).map_err(map_db_error)?;
                Ok(v.map(|x| x.normalize().to_string()))
            };
            put(&mut m, "student_number", get("student_number")?);
            put(&mut m, "term_label", get("term_label")?);
            put(&mut m, "academic_year_label", get("academic_year_label")?);
            put(&mut m, "course_code", get("course_code")?);
            put(&mut m, "course_title", get("course_title")?);
            put(&mut m, "credits", dec("credits")?);
            put(&mut m, "grade", get("grade")?);
            put(&mut m, "grade_point", dec("grade_point")?);
            put(&mut m, "marks", dec("marks")?);
            put(&mut m, "max_marks", dec("max_marks")?);
            put(&mut m, "result_status", get("result_status")?);
            let a: i32 = r.try_get("attempt").map_err(map_db_error)?;
            put(&mut m, "attempt", Some(a.to_string()));
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
    fn student_rows_use_the_domain_validators() {
        let ok = student_cells(&row(&[
            ("student_number", "S2023/0001"),
            ("given_name", "Aarav"),
            ("family_name", "Desai"),
            ("programme_code", "BTECH-CSE"),
            ("guardian_name", "Meena Desai"),
            ("guardian_relationship", "mother"),
            ("guardian_phone", "+91 98200 00001"),
        ]))
        .unwrap();
        let g = ok.guardian.unwrap();
        assert_eq!(
            (g.given_name.as_str(), g.family_name.as_deref()),
            ("Meena", Some("Desai"))
        );
        assert_eq!(ok.enrolment_type, "regular");
        assert_eq!(ok.current_term_no, 1);
        let Err(KernelError::Validation(v)) = student_cells(&row(&[
            ("student_number", "bad number!"),
            ("email", "not-an-email"),
            ("guardian_name", "X"),
            ("guardian_relationship", "uncle"),
        ])) else {
            panic!("expected validation errors");
        };
        let fields: Vec<&str> = v.iter().map(|f| f.field.as_str()).collect();
        for f in [
            "student_number",
            "given_name",
            "email",
            "programme_code",
            "guardian_phone",
            "guardian_relationship",
        ] {
            assert!(fields.contains(&f), "{f} missing from {fields:?}");
        }
    }

    #[test]
    fn result_rows_are_checked_and_keyed_with_default_attempt() {
        let base = [
            ("student_number", "S1"),
            ("term_label", "Sem 1"),
            ("course_code", "CS101"),
            ("course_title", "Programming"),
            ("credits", "4"),
            ("result_status", "pass"),
        ];
        assert_eq!(result_cells(&row(&base)).unwrap().attempt, 1);
        let mut bad = base.to_vec();
        bad.push(("marks", "120"));
        bad.push(("max_marks", "100"));
        assert!(result_cells(&row(&bad)).is_err());
        assert_eq!(results_spec().natural_key.len(), 4);
        assert!(students_spec().column("programme_code").unwrap().required);
    }
}
