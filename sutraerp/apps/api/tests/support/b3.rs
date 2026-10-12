//! Batch 3 foundation test helpers (foundation-owned; slices add their own helpers in their own
//! test file, see `docs/batch3-contracts.md` §11).
//!
//! * [`academic_setup`] — an institution with a published programme + curriculum version and a
//!   published calendar (two terms), all through the real Domain B HTTP API.
//! * [`seed_student`] — inserts a Student persona + **active** StudentContext directly into the
//!   foundation tables (owner pool). 3B and 3C use it so their tests do not depend on 3A's
//!   admission flow; 3A tests must create students through its own API instead.

use super::{b2, register_institution, Login};
use axum::Router;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Domain B context a Domain C test needs.
#[derive(Clone, Debug)]
pub struct AcademicSetup {
    pub tenant_id: Uuid,
    pub institution_id: Uuid,
    pub programme_id: Uuid,
    pub curriculum_version_id: Uuid,
    pub calendar_version_id: Uuid,
    pub academic_year_id: Uuid,
    /// Term ids in sequence order (T1, T2).
    pub term_ids: Vec<Uuid>,
    pub programme: Value,
    pub curriculum_version: Value,
    pub calendar_version: Value,
}

fn uuid(v: &Value, what: &str) -> Uuid {
    v.as_str()
        .unwrap_or_else(|| panic!("no {what} in {v}"))
        .parse()
        .unwrap()
}

/// Registers an institution (left in `draft`, which is writable for Domain C) and publishes a
/// programme/curriculum and a calendar in it.
pub async fn academic_setup(
    app: &Router,
    admin: &Login,
    tenant: &str,
    name: &str,
) -> AcademicSetup {
    let inst = register_institution(app, admin, name).await;
    let (programme, cv) = b2::published_curriculum(app, admin, &inst).await;
    let (_cal, version) = b2::published_calendar(app, admin, &inst).await;
    let mut terms: Vec<&Value> = version["terms"].as_array().expect("terms").iter().collect();
    terms.sort_by_key(|t| t["sequence"].as_i64());
    AcademicSetup {
        tenant_id: tenant.parse().unwrap(),
        institution_id: inst.parse().unwrap(),
        programme_id: uuid(&programme["id"], "programme id"),
        curriculum_version_id: uuid(&cv["id"], "curriculum version id"),
        calendar_version_id: uuid(&version["id"], "calendar version id"),
        academic_year_id: uuid(&version["academicYearId"], "academic year id"),
        term_ids: terms.iter().map(|t| uuid(&t["id"], "term id")).collect(),
        programme,
        curriculum_version: cv,
        calendar_version: version,
    }
}

/// A seeded student: persona + its active context.
#[derive(Clone, Debug)]
pub struct SeededStudent {
    pub student_id: Uuid,
    pub context_id: Uuid,
    pub student_number: String,
}

/// Inserts an `active` persona with an `active` context in `setup`'s institution/programme
/// (term 1). `subject_ref` links a login for self-service tests, e.g.
/// `Some("dev|aarav.desai@sutra.dev")` (only one non-archived student per subject per tenant).
pub async fn seed_student(
    owner: &PgPool,
    setup: &AcademicSetup,
    given_name: &str,
    subject_ref: Option<&str>,
) -> SeededStudent {
    let number = format!(
        "S{}",
        Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    );
    let student_id: Uuid = sqlx::query_scalar(
        "INSERT INTO student.student (tenant_id, student_number, given_name, family_name, \
         subject_ref, status) VALUES ($1, $2, $3, 'Seeded', $4, 'active') RETURNING id",
    )
    .bind(setup.tenant_id)
    .bind(&number)
    .bind(given_name)
    .bind(subject_ref)
    .fetch_one(owner)
    .await
    .expect("seed student");
    let context_id: Uuid = sqlx::query_scalar(
        "INSERT INTO student.student_context (tenant_id, student_id, institution_id, programme_id, \
         curriculum_version_id, admission_term_id, admission_year_id, current_term_no, \
         enrolment_number, enrolment_type, enrolled_on, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, 'regular', DATE '2026-06-15', 'active') \
         RETURNING id",
    )
    .bind(setup.tenant_id)
    .bind(student_id)
    .bind(setup.institution_id)
    .bind(setup.programme_id)
    .bind(setup.curriculum_version_id)
    .bind(setup.term_ids.first().copied())
    .bind(setup.academic_year_id)
    .bind(format!("EN-{number}"))
    .fetch_one(owner)
    .await
    .expect("seed context");
    SeededStudent {
        student_id,
        context_id,
        student_number: number,
    }
}
