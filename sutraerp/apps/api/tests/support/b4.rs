//! Batch 4 foundation test helpers (foundation-owned; slices add their own helpers in their own
//! test file, see `docs/batch4-contracts.md` §11).
//!
//! * [`academic_setup`] — Batch 3's Domain B setup (institution + published programme/curriculum
//!   + published calendar with two terms) plus the curriculum components of term 1.
//! * [`seed_offering`] — a course offering (any status) with one section `A`, inserted directly
//!   into the foundation tables (owner pool). 4B/4C use it so they never wait for 4A's API; 4A
//!   tests create offerings through its own API instead.
//! * [`seed_faculty`] — a faculty assignment (default `active`) for a login subject
//!   ([`MEERA`] = the dev faculty persona).
//! * [`seed_student`] — Batch 3's seeded active student; [`seed_enrolment`] seats it in a section.
//! * [`seed_registration`] — a 3B registration (window + registration + lines) so 4A's
//!   `RegistrationReader` gating can be tested without 3B's API.

use super::b3::{self, AcademicSetup, SeededStudent};
use super::Login;
use axum::Router;
use sqlx::PgPool;
use sutra_contracts::institution::reference::{CurriculumComponentRef, InstitutionReferencePort};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use uuid::Uuid;

/// Login subject of the dev faculty persona (`meera.shah@sutra.dev`).
pub const MEERA: &str = "dev|meera.shah@sutra.dev";
/// Login subject of the dev student persona (`aarav.desai@sutra.dev`).
pub const AARAV: &str = "dev|aarav.desai@sutra.dev";

/// Domain B context a Domain D test needs.
#[derive(Clone, Debug)]
pub struct DeliverySetup {
    pub base: AcademicSetup,
    /// Term 1 of the published calendar (offerings are seeded in it).
    pub term_id: Uuid,
    /// Curriculum components of programme term 1 (`CS101` core 12 cr, `MD101` 8 cr).
    pub components: Vec<CurriculumComponentRef>,
}

impl DeliverySetup {
    pub fn tenant(&self) -> TenantId {
        TenantId(self.base.tenant_id)
    }
    pub fn institution(&self) -> InstitutionId {
        InstitutionId(self.base.institution_id)
    }
    pub fn scope(&self) -> ExecutionScope {
        ExecutionScope::institution(self.tenant(), self.institution())
    }
    /// The term-1 component with `slot_code` (panics when missing).
    pub fn component(&self, slot_code: &str) -> &CurriculumComponentRef {
        self.components
            .iter()
            .find(|c| c.slot_code == slot_code)
            .unwrap_or_else(|| panic!("no component {slot_code}"))
    }
}

pub async fn academic_setup(
    app: &Router,
    admin: &Login,
    owner: &PgPool,
    tenant: &str,
    name: &str,
) -> DeliverySetup {
    let base = b3::academic_setup(app, admin, tenant, name).await;
    let refs = sutra_institution::PgInstitutionReferences::new(owner.clone());
    let scope =
        ExecutionScope::institution(TenantId(base.tenant_id), InstitutionId(base.institution_id));
    let components = refs
        .curriculum_components(&scope, base.curriculum_version_id)
        .await
        .expect("components")
        .into_iter()
        .filter(|c| c.term_no == 1)
        .collect();
    DeliverySetup {
        term_id: base.term_ids[0],
        base,
        components,
    }
}

/// A seeded offering with its first section.
#[derive(Clone, Debug)]
pub struct SeededOffering {
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub component_id: Uuid,
    pub offering_code: String,
}

/// Inserts an offering of the term-1 component `slot_code` in `status` (e.g. `active`) with one
/// section `A` (`active` unless the offering is still being planned).
pub async fn seed_offering(
    owner: &PgPool,
    setup: &DeliverySetup,
    slot_code: &str,
    status: &str,
) -> SeededOffering {
    let c = setup.component(slot_code).clone();
    let code = format!(
        "{slot_code}-{}",
        Uuid::now_v7().simple().to_string()[24..].to_uppercase()
    );
    let offering_id: Uuid = sqlx::query_scalar(
        "INSERT INTO academic.course_offering (tenant_id, institution_id, academic_year_id, \
         academic_term_id, curriculum_version_id, curriculum_component_id, offering_code, title, \
         course_code, category, credits, capacity, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 60, $12) RETURNING id",
    )
    .bind(setup.base.tenant_id)
    .bind(setup.base.institution_id)
    .bind(setup.base.academic_year_id)
    .bind(setup.term_id)
    .bind(setup.base.curriculum_version_id)
    .bind(c.id)
    .bind(&code)
    .bind(&c.title)
    .bind(&c.course_code)
    .bind(&c.category)
    .bind(c.credits)
    .bind(status)
    .fetch_one(owner)
    .await
    .expect("seed offering");
    let section_status = if matches!(status, "draft" | "pending_approval" | "approved") {
        "planned"
    } else {
        "active"
    };
    let section_id = seed_section(owner, setup, offering_id, "A", section_status).await;
    SeededOffering {
        offering_id,
        section_id,
        component_id: c.id,
        offering_code: code,
    }
}

/// Inserts a section of an offering.
pub async fn seed_section(
    owner: &PgPool,
    setup: &DeliverySetup,
    offering_id: Uuid,
    code: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, \
         section_code, capacity, status) VALUES ($1, $2, $3, $4, 60, $5) RETURNING id",
    )
    .bind(setup.base.tenant_id)
    .bind(setup.base.institution_id)
    .bind(offering_id)
    .bind(code)
    .bind(status)
    .fetch_one(owner)
    .await
    .expect("seed section")
}

/// Inserts a faculty assignment of `subject` on the offering's section (`status` e.g. `active`).
pub async fn seed_faculty(
    owner: &PgPool,
    setup: &DeliverySetup,
    offering: &SeededOffering,
    subject: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, \
         section_id, faculty_subject_ref, faculty_name, role, weekly_hours, effective_from, status) \
         VALUES ($1, $2, $3, $4, $5, $6, 'primary_instructor', 4, DATE '2026-06-15', $7) RETURNING id",
    )
    .bind(setup.base.tenant_id)
    .bind(setup.base.institution_id)
    .bind(offering.offering_id)
    .bind(offering.section_id)
    .bind(subject)
    .bind(if subject == MEERA {
        "Dr. Meera Shah"
    } else {
        "Seeded Faculty"
    })
    .bind(status)
    .fetch_one(owner)
    .await
    .expect("seed faculty assignment")
}

/// Batch 3's seeded active student (persona + active context, term 1).
pub async fn seed_student(
    owner: &PgPool,
    setup: &DeliverySetup,
    given_name: &str,
    subject_ref: Option<&str>,
) -> SeededStudent {
    b3::seed_student(owner, &setup.base, given_name, subject_ref).await
}

/// Seats a student in the offering's section (`enrolled`, origin `override`).
pub async fn seed_enrolment(
    owner: &PgPool,
    setup: &DeliverySetup,
    offering: &SeededOffering,
    student: &SeededStudent,
    subject_ref: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, \
         section_id, student_id, student_context_id, student_number, display_name, subject_ref, \
         origin, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'override', 'enrolled') \
         RETURNING id",
    )
    .bind(setup.base.tenant_id)
    .bind(setup.base.institution_id)
    .bind(offering.offering_id)
    .bind(offering.section_id)
    .bind(student.student_id)
    .bind(student.context_id)
    .bind(&student.student_number)
    .bind(format!("Seeded {}", student.student_number))
    .bind(subject_ref)
    .fetch_one(owner)
    .await
    .expect("seed section enrolment")
}

/// Inserts a 3B registration (`status`, e.g. `registered`) for term 1 with one `registered`
/// line per component; creates the term's registration window when missing. Returns the
/// registration id.
pub async fn seed_registration(
    owner: &PgPool,
    setup: &DeliverySetup,
    student: &SeededStudent,
    component_ids: &[Uuid],
    status: &str,
) -> Uuid {
    let window: Uuid = match sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM student.registration_window WHERE tenant_id = $1 AND institution_id = $2 \
         AND academic_term_id = $3 ORDER BY created_at LIMIT 1",
    )
    .bind(setup.base.tenant_id)
    .bind(setup.base.institution_id)
    .bind(setup.term_id)
    .fetch_optional(owner)
    .await
    .expect("window lookup")
    {
        Some(w) => w,
        None => sqlx::query_scalar(
            "INSERT INTO student.registration_window (tenant_id, institution_id, academic_term_id, \
             name, opens_on, closes_on, status) VALUES ($1, $2, $3, 'Term 1 registration', \
             DATE '2026-06-01', DATE '2026-07-15', 'open') RETURNING id",
        )
        .bind(setup.base.tenant_id)
        .bind(setup.base.institution_id)
        .bind(setup.term_id)
        .fetch_one(owner)
        .await
        .expect("seed window"),
    };
    let reg: Uuid = sqlx::query_scalar(
        "INSERT INTO student.registration (tenant_id, institution_id, student_id, \
         student_context_id, academic_term_id, registration_window_id, term_no, \
         curriculum_version_id, status) VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $8) RETURNING id",
    )
    .bind(setup.base.tenant_id)
    .bind(setup.base.institution_id)
    .bind(student.student_id)
    .bind(student.context_id)
    .bind(setup.term_id)
    .bind(window)
    .bind(setup.base.curriculum_version_id)
    .bind(status)
    .fetch_one(owner)
    .await
    .expect("seed registration");
    for cid in component_ids {
        let c = setup
            .components
            .iter()
            .find(|c| c.id == *cid)
            .expect("term-1 component");
        sqlx::query(
            "INSERT INTO student.registration_line (tenant_id, registration_id, \
             curriculum_component_id, kind, status, credits, component_term_no, slot_code, title) \
             VALUES ($1, $2, $3, 'regular', 'registered', $4, 1, $5, $6)",
        )
        .bind(setup.base.tenant_id)
        .bind(reg)
        .bind(c.id)
        .bind(c.credits)
        .bind(&c.slot_code)
        .bind(&c.title)
        .execute(owner)
        .await
        .expect("seed registration line");
    }
    reg
}
