//! **2B** — academic structure, programme catalogue, curriculum framework versions, school
//! academic model (L2-B04 + curriculum-retirement amendment 2026-08-24).

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// Structure version published ([`AcademicStructurePublishedV1`]). Read by 2C readiness.
    ACADEMIC_STRUCTURE_PUBLISHED, ACADEMIC_STRUCTURE_PUBLISHED_SCHEMA,
    "academic_structure.published");
event_type!(
    /// Structure retired (StatusChangedV1) — `AcademicStructureRetired`.
    ACADEMIC_STRUCTURE_RETIRED, ACADEMIC_STRUCTURE_RETIRED_SCHEMA, "academic_structure.retired");
event_type!(
    /// [`ProgrammeV1`] — registered in the catalogue (Draft).
    PROGRAMME_REGISTERED, PROGRAMME_REGISTERED_SCHEMA, "programme.registered");
event_type!(
    /// [`ProgrammeV1`] — published (offerable). Read by 2C readiness.
    PROGRAMME_PUBLISHED, PROGRAMME_PUBLISHED_SCHEMA, "programme.published");
event_type!(
    /// Programme retired (StatusChangedV1).
    PROGRAMME_RETIRED, PROGRAMME_RETIRED_SCHEMA, "programme.retired");
event_type!(
    /// Immutable curriculum version published ([`CurriculumVersionPublishedV1`]); a successor
    /// supersedes its predecessor in the same transaction. Read by 2C readiness.
    CURRICULUM_VERSION_PUBLISHED, CURRICULUM_VERSION_PUBLISHED_SCHEMA,
    "curriculum_version.published");
event_type!(
    /// Curriculum version retired (StatusChangedV1) — `CurriculumVersionRetired`.
    CURRICULUM_VERSION_RETIRED, CURRICULUM_VERSION_RETIRED_SCHEMA, "curriculum_version.retired");
event_type!(
    /// Whole curriculum framework retired (StatusChangedV1, `aggregateId` = curriculum id) —
    /// `CurriculumRetired`.
    CURRICULUM_RETIRED, CURRICULUM_RETIRED_SCHEMA, "curriculum.retired");
event_type!(
    /// School academic model published ([`SchoolAcademicModelPublishedV1`]). Read by 2C readiness.
    SCHOOL_MODEL_PUBLISHED, SCHOOL_MODEL_PUBLISHED_SCHEMA, "school_model.published");

pub const EVENT_TYPES: &[&str] = &[
    ACADEMIC_STRUCTURE_PUBLISHED,
    ACADEMIC_STRUCTURE_RETIRED,
    PROGRAMME_REGISTERED,
    PROGRAMME_PUBLISHED,
    PROGRAMME_RETIRED,
    CURRICULUM_VERSION_PUBLISHED,
    CURRICULUM_VERSION_RETIRED,
    CURRICULUM_RETIRED,
    SCHOOL_MODEL_PUBLISHED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcademicStructurePublishedV1 {
    pub academic_structure_id: Uuid,
    pub institution_id: Uuid,
    pub version_no: i32,
    pub supersedes_id: Option<Uuid>,
    pub node_count: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammeV1 {
    pub programme_id: Uuid,
    pub institution_id: Uuid,
    /// Owning organisation unit (2A), when assigned.
    pub org_unit_id: Option<Uuid>,
    pub code: String,
    pub name: String,
    /// Academic level key from the academic structure (e.g. `ug`, `pg`, `grade`).
    pub level: String,
    pub duration_terms: i32,
    pub version: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurriculumVersionPublishedV1 {
    pub curriculum_id: Uuid,
    pub curriculum_version_id: Uuid,
    pub programme_id: Uuid,
    pub institution_id: Uuid,
    pub version_no: i32,
    /// Predecessor version superseded by this publication.
    pub supersedes_version_id: Option<Uuid>,
    /// Business-local date from which new cohorts follow this version.
    pub effective_from: NaiveDate,
    pub total_credits: Option<String>,
    pub process_instance_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchoolAcademicModelPublishedV1 {
    pub school_model_id: Uuid,
    pub institution_id: Uuid,
    pub version_no: i32,
    /// Ordered grade level keys, e.g. `["nursery", "1", …, "12"]`.
    pub grade_levels: Vec<String>,
    /// Board / affiliation key, e.g. `cbse`.
    pub board: Option<String>,
}
