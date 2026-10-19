//! Curriculum framework (L2-B04, L3-B-023/024/025): a curriculum per programme lineage with
//! immutable versions. A version carries the term structure, credit rules, NEP 2020 alignment
//! (NHEQF level, multiple entry/exit with exit awards, Academic Bank of Credits) and its course
//! slots. Revision creates a successor draft linked to the published predecessor; publishing it
//! supersedes the predecessor in the same transaction. Existing cohorts never migrate
//! automatically: a version applies from its effective date / cohort.

use super::domain::{ensure, ensure_editable, nheqf_ok, parse_status};
use super::programme;
use super::publication::{self, head, move_status, supersede_current, Kind};
use super::structure::PublicationQuery;
use super::support::{
    begin, body, code_format, expect_version, finish, half_credits, institution_of, required_text,
    violation, DecisionBody, ReasonBody, Transition,
};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::{publish, Actor, InstitutionState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::HashSet;
use sutra_contracts::institution::events::curriculum::{
    CurriculumVersionPublishedV1, CURRICULUM_RETIRED, CURRICULUM_RETIRED_SCHEMA,
    CURRICULUM_VERSION_PUBLISHED, CURRICULUM_VERSION_PUBLISHED_SCHEMA, CURRICULUM_VERSION_RETIRED,
    CURRICULUM_VERSION_RETIRED_SCHEMA,
};
use sutra_contracts::institution::ids::{CurriculumId, CurriculumVersionId, ProgrammeId};
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

const CURRICULUM: &str = "curriculum";
const VERSION: &str = "curriculum_version";

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TermStructure {
    Semester,
    Trimester,
    Annual,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CreditFramework {
    /// NEP 2020 / UGC curriculum and credit framework.
    Nep2020,
    /// Choice Based Credit System.
    Cbcs,
    Custom,
}

/// NEP 2020 course categories of a slot.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ComponentCategory {
    MajorCore,
    MajorElective,
    Minor,
    Multidisciplinary,
    AbilityEnhancement,
    SkillEnhancement,
    ValueAdded,
    Internship,
    ResearchProject,
    OpenElective,
}

macro_rules! wire {
    ($t:ty { $($v:ident => $s:literal),+ $(,)? }) => {
        impl $t {
            pub fn as_str(self) -> &'static str { match self { $(Self::$v => $s),+ } }
            pub fn parse(s: &str) -> Option<Self> { match s { $($s => Some(Self::$v),)+ _ => None } }
        }
    };
}
wire!(TermStructure { Semester => "semester", Trimester => "trimester", Annual => "annual" });
wire!(CreditFramework { Nep2020 => "nep2020", Cbcs => "cbcs", Custom => "custom" });
wire!(ComponentCategory {
    MajorCore => "major_core", MajorElective => "major_elective", Minor => "minor",
    Multidisciplinary => "multidisciplinary", AbilityEnhancement => "ability_enhancement",
    SkillEnhancement => "skill_enhancement", ValueAdded => "value_added",
    Internship => "internship", ResearchProject => "research_project",
    OpenElective => "open_elective",
});

/// NEP multiple entry / exit: the award a student may exit with after `afterTerms` terms.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitAward {
    #[schema(example = 2)]
    pub after_terms: i32,
    #[schema(example = "UG Certificate")]
    pub award: String,
    #[schema(example = 40)]
    pub min_credits: f64,
}

/// One course slot of a curriculum version.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurriculumComponent {
    /// 1-based term (semester) number.
    pub term_no: i32,
    #[schema(example = "CS101")]
    pub slot_code: String,
    pub title: String,
    pub category: ComponentCategory,
    /// Credits in half-credit steps.
    pub credits: f64,
    #[serde(default)]
    pub lecture_hours: i32,
    #[serde(default)]
    pub tutorial_hours: i32,
    #[serde(default)]
    pub practical_hours: i32,
    /// Elective basket the slot belongs to.
    pub basket_code: Option<String>,
    /// Course master code, when bound.
    pub course_code: Option<String>,
}

/// Editable content of a curriculum version (create, revise, `PATCH` full replacement).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurriculumVersionContent {
    /// Business-local date from which new cohorts follow this version.
    pub effective_from: NaiveDate,
    /// Academic-year label of the first cohort that follows it (existing cohorts never migrate).
    pub applies_from_cohort: Option<String>,
    pub change_summary: Option<String>,
    pub term_structure: TermStructure,
    pub term_count: i32,
    pub credit_framework: CreditFramework,
    pub min_total_credits: Option<f64>,
    pub min_credits_per_term: Option<f64>,
    pub max_credits_per_term: Option<f64>,
    #[serde(default)]
    pub nep_aligned: bool,
    pub nheqf_level: Option<f64>,
    #[serde(default)]
    pub multiple_entry_exit: bool,
    /// Academic Bank of Credits participation.
    #[serde(default)]
    pub abc_enabled: bool,
    #[serde(default)]
    pub exit_awards: Vec<ExitAward>,
    #[serde(default)]
    pub components: Vec<CurriculumComponent>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CurriculumVersion {
    pub id: CurriculumVersionId,
    pub curriculum_id: CurriculumId,
    pub programme_id: ProgrammeId,
    pub institution_id: InstitutionId,
    pub version_no: i32,
    pub status: PublicationStatus,
    pub supersedes_version_id: Option<CurriculumVersionId>,
    #[serde(flatten)]
    pub content: CurriculumVersionContent,
    /// Sum of the slot credits.
    pub total_credits: f64,
    pub process_instance_id: Option<String>,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CurriculumVersionList {
    pub items: Vec<CurriculumVersion>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Curriculum {
    pub id: CurriculumId,
    pub institution_id: InstitutionId,
    pub programme_id: ProgrammeId,
    pub code: String,
    pub title: String,
    /// `draft` until the first version is published; `retired` once the curriculum is retired.
    pub status: PublicationStatus,
    /// The currently published version, if any.
    pub current_version_id: Option<CurriculumVersionId>,
    /// The open draft / in-review version, if any.
    pub open_version_id: Option<CurriculumVersionId>,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CurriculumList {
    pub items: Vec<Curriculum>,
}

/// `POST /programmes/{id}/curricula`: the curriculum and its first draft version.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCurriculum {
    #[schema(example = "BTECH-CSE-2026")]
    pub code: String,
    pub title: String,
    pub initial_version: CurriculumVersionContent,
}

/// `POST /curricula/{id}/revisions`: successor draft of the published version (content is copied
/// and then edited with `PATCH /curriculum-versions/{id}`).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviseCurriculum {
    /// What changes and why (academic / regulatory basis).
    pub change_summary: String,
    /// Must not precede the predecessor's effective date.
    pub effective_from: NaiveDate,
    pub applies_from_cohort: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

fn credits_field(field: &str, value: Option<f64>, v: &mut Vec<FieldViolation>) {
    if let Some(c) = value {
        if half_credits(c).is_none() || c > 9999.0 {
            v.push(violation(
                field,
                "format",
                "credits are non-negative, in half-credit steps",
            ));
        }
    }
}

impl CurriculumVersionContent {
    /// Shape rules (draft time).
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        if !(1..=24).contains(&self.term_count) {
            v.push(violation("termCount", "range", "term count must be 1-24"));
        }
        if let Some(c) = &self.applies_from_cohort {
            required_text("appliesFromCohort", c, 20, &mut v);
        }
        if let Some(s) = &self.change_summary {
            if s.chars().count() > 2000 {
                v.push(violation(
                    "changeSummary",
                    "too-long",
                    "at most 2000 characters",
                ));
            }
        }
        credits_field("minTotalCredits", self.min_total_credits, &mut v);
        credits_field("minCreditsPerTerm", self.min_credits_per_term, &mut v);
        credits_field("maxCreditsPerTerm", self.max_credits_per_term, &mut v);
        if let (Some(lo), Some(hi)) = (self.min_credits_per_term, self.max_credits_per_term) {
            if lo > hi {
                v.push(violation(
                    "maxCreditsPerTerm",
                    "range",
                    "maximum credits per term is below the minimum",
                ));
            }
        }
        if let Some(l) = self.nheqf_level {
            if !nheqf_ok(l) {
                v.push(violation(
                    "nheqfLevel",
                    "range",
                    "NHEQF level is 4.5-8 in half steps",
                ));
            }
        }
        if self.multiple_entry_exit && self.exit_awards.is_empty() {
            v.push(violation(
                "exitAwards",
                "required",
                "multiple entry / exit needs at least one exit award",
            ));
        }
        if !self.multiple_entry_exit && !self.exit_awards.is_empty() {
            v.push(violation(
                "exitAwards",
                "inconsistent",
                "exit awards require multipleEntryExit",
            ));
        }
        let mut last: Option<(i32, f64)> = None;
        for (i, a) in self.exit_awards.iter().enumerate() {
            let f = |m: &str| format!("exitAwards[{i}].{m}");
            required_text(&f("award"), &a.award, 120, &mut v);
            if a.after_terms < 1 || a.after_terms >= self.term_count {
                v.push(violation(
                    &f("afterTerms"),
                    "range",
                    "an exit lies strictly inside the programme",
                ));
            }
            credits_field(&f("minCredits"), Some(a.min_credits), &mut v);
            if let Some((t, c)) = last {
                if a.after_terms <= t || a.min_credits <= c {
                    v.push(violation(
                        &f("afterTerms"),
                        "order",
                        "exit awards must increase in terms and credits",
                    ));
                }
            }
            last = Some((a.after_terms, a.min_credits));
        }
        if self.components.len() > 1000 {
            v.push(violation("components", "too-many", "at most 1000 slots"));
        }
        let mut slots = HashSet::new();
        for (i, c) in self.components.iter().enumerate() {
            let f = |m: &str| format!("components[{i}].{m}");
            code_format(&f("slotCode"), &c.slot_code, &mut v);
            if !slots.insert(c.slot_code.trim()) {
                v.push(violation(
                    &f("slotCode"),
                    "duplicate",
                    "slot code is used twice",
                ));
            }
            required_text(&f("title"), &c.title, 200, &mut v);
            // Course titles are identity text (audit 2026-10 A2, VAL-1): no markup / controls.
            if let Some(x) = sutra_kernel::text::identity_chars(&f("title"), c.title.trim()) {
                v.push(x);
            }
            if c.term_no < 1 || c.term_no > self.term_count {
                v.push(violation(
                    &f("termNo"),
                    "range",
                    "term number is outside the term count",
                ));
            }
            if half_credits(c.credits).is_none_or(|h| h == 0) || c.credits > 99.0 {
                v.push(violation(
                    &f("credits"),
                    "format",
                    "credits are positive, in half-credit steps",
                ));
            }
            for (name, h) in [
                ("lectureHours", c.lecture_hours),
                ("tutorialHours", c.tutorial_hours),
                ("practicalHours", c.practical_hours),
            ] {
                if !(0..=40).contains(&h) {
                    v.push(violation(&f(name), "range", "weekly hours are 0-40"));
                }
            }
            if let Some(b) = &c.basket_code {
                code_format(&f("basketCode"), b, &mut v);
            }
            if let Some(cc) = &c.course_code {
                code_format(&f("courseCode"), cc, &mut v);
            }
        }
        finish(v)
    }

    /// Half-credits per term (index 0 = term 1).
    fn per_term(&self) -> Vec<i64> {
        let n = usize::try_from(self.term_count.max(0)).unwrap_or(0);
        let mut terms = vec![0_i64; n];
        for c in &self.components {
            if let Some(slot) = usize::try_from(c.term_no - 1)
                .ok()
                .and_then(|i| terms.get_mut(i))
            {
                *slot += half_credits(c.credits).unwrap_or(0);
            }
        }
        terms
    }

    pub fn total_credits(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let half = self.per_term().iter().sum::<i64>() as f64;
        half / 2.0
    }

    /// Credit rules checked at publication (L4-B-023-BH-004): every term has slots, per-term
    /// limits hold, the total reaches the minimum, exit awards are reachable, NEP alignment is
    /// complete, and the term count matches the programme duration.
    pub fn publication_rules(&self, programme_duration: i32) -> Vec<FieldViolation> {
        let mut v = vec![];
        if self.components.is_empty() {
            v.push(violation(
                "components",
                "required-for-publication",
                "at least one slot is required",
            ));
        }
        if self.term_count != programme_duration {
            v.push(violation(
                "termCount",
                "programme-mismatch",
                format!("term count must equal the programme duration ({programme_duration})"),
            ));
        }
        let terms = self.per_term();
        let lo = self.min_credits_per_term.and_then(half_credits);
        let hi = self.max_credits_per_term.and_then(half_credits);
        for (i, t) in terms.iter().enumerate() {
            let term = i + 1;
            if *t == 0 {
                v.push(violation(
                    "components",
                    "empty-term",
                    format!("term {term} has no slots"),
                ));
            }
            if lo.is_some_and(|lo| *t < lo) || hi.is_some_and(|hi| *t > hi) {
                v.push(violation(
                    "components",
                    "term-credits",
                    format!(
                        "term {term} has {} credits, outside the per-term limits",
                        *t as f64 / 2.0
                    ),
                ));
            }
        }
        let total: i64 = terms.iter().sum();
        if self
            .min_total_credits
            .and_then(half_credits)
            .is_some_and(|m| total < m)
        {
            v.push(violation(
                "minTotalCredits",
                "total-credits",
                format!(
                    "slots total {} credits, below the minimum",
                    total as f64 / 2.0
                ),
            ));
        }
        for (i, a) in self.exit_awards.iter().enumerate() {
            let upto: i64 = terms
                .iter()
                .take(usize::try_from(a.after_terms.max(0)).unwrap_or(0))
                .sum();
            if half_credits(a.min_credits).is_some_and(|m| upto < m) {
                v.push(violation(
                    &format!("exitAwards[{i}].minCredits"),
                    "unreachable",
                    "the slots up to this exit do not carry the required credits",
                ));
            }
        }
        if self.nep_aligned {
            if self.credit_framework != CreditFramework::Nep2020 {
                v.push(violation(
                    "creditFramework",
                    "nep",
                    "NEP-aligned versions use the nep2020 credit framework",
                ));
            }
            if self.nheqf_level.is_none() {
                v.push(violation(
                    "nheqfLevel",
                    "nep",
                    "NEP-aligned versions state their NHEQF level",
                ));
            }
            if !self
                .components
                .iter()
                .any(|c| c.category == ComponentCategory::MajorCore)
            {
                v.push(violation(
                    "components",
                    "nep",
                    "NEP-aligned versions need major core slots",
                ));
            }
        }
        v
    }
}

impl CreateCurriculum {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        code_format("code", &self.code, &mut v);
        required_text("title", &self.title, 200, &mut v);
        finish(v)?;
        self.initial_version.validate()
    }
}

// ---------------------------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------------------------

const V_COLUMNS: &str = "id, curriculum_id, programme_id, institution_id, version_no, status, \
    supersedes_version_id, change_summary, effective_from, applies_from_cohort, term_structure, \
    term_count, credit_framework, min_total_credits::float8 AS min_total_credits, \
    min_credits_per_term::float8 AS min_credits_per_term, \
    max_credits_per_term::float8 AS max_credits_per_term, nep_aligned, \
    nheqf_level::float8 AS nheqf_level, multiple_entry_exit, abc_enabled, exit_awards, \
    process_instance_id, status_reason, published_at, retired_at, version, created_at, updated_at";

fn decode<T>(v: Option<T>, what: &str, raw: &str) -> Result<T, sqlx::Error> {
    v.ok_or_else(|| sqlx::Error::Decode(format!("{what} `{raw}`").into()))
}

fn version_from_row(r: &PgRow) -> Result<CurriculumVersion, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let ts: String = r.try_get("term_structure")?;
    let cf: String = r.try_get("credit_framework")?;
    let awards: serde_json::Value = r.try_get("exit_awards")?;
    Ok(CurriculumVersion {
        id: CurriculumVersionId(r.try_get("id")?),
        curriculum_id: CurriculumId(r.try_get("curriculum_id")?),
        programme_id: ProgrammeId(r.try_get("programme_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        version_no: r.try_get("version_no")?,
        status: parse_status(&status)?,
        supersedes_version_id: r
            .try_get::<Option<Uuid>, _>("supersedes_version_id")?
            .map(CurriculumVersionId),
        content: CurriculumVersionContent {
            effective_from: r.try_get("effective_from")?,
            applies_from_cohort: r.try_get("applies_from_cohort")?,
            change_summary: r.try_get("change_summary")?,
            term_structure: decode(TermStructure::parse(&ts), "term structure", &ts)?,
            term_count: r.try_get("term_count")?,
            credit_framework: decode(CreditFramework::parse(&cf), "credit framework", &cf)?,
            min_total_credits: r.try_get("min_total_credits")?,
            min_credits_per_term: r.try_get("min_credits_per_term")?,
            max_credits_per_term: r.try_get("max_credits_per_term")?,
            nep_aligned: r.try_get("nep_aligned")?,
            nheqf_level: r.try_get("nheqf_level")?,
            multiple_entry_exit: r.try_get("multiple_entry_exit")?,
            abc_enabled: r.try_get("abc_enabled")?,
            exit_awards: serde_json::from_value(awards)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            components: vec![],
        },
        total_credits: 0.0,
        process_instance_id: r.try_get("process_instance_id")?,
        status_reason: r.try_get("status_reason")?,
        published_at: r.try_get("published_at")?,
        retired_at: r.try_get("retired_at")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn components_of(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<CurriculumComponent>, KernelError> {
    let rows = sqlx::query(
        "SELECT term_no, slot_code, title, category, credits::float8 AS credits, lecture_hours, \
         tutorial_hours, practical_hours, basket_code, course_code FROM institution.curriculum_component \
         WHERE curriculum_version_id = $1 ORDER BY term_no, sort_order, slot_code",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let cat: String = r.try_get("category")?;
            Ok(CurriculumComponent {
                term_no: r.try_get("term_no")?,
                slot_code: r.try_get("slot_code")?,
                title: r.try_get("title")?,
                category: decode(ComponentCategory::parse(&cat), "category", &cat)?,
                credits: r.try_get("credits")?,
                lecture_hours: r.try_get("lecture_hours")?,
                tutorial_hours: r.try_get("tutorial_hours")?,
                practical_hours: r.try_get("practical_hours")?,
                basket_code: r.try_get("basket_code")?,
                course_code: r.try_get("course_code")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

pub(crate) async fn load_version(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<CurriculumVersion, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {V_COLUMNS} FROM institution.curriculum_version WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: VERSION })?;
    let mut v = version_from_row(&row).map_err(map_db_error)?;
    v.content.components = components_of(conn, id).await?;
    v.total_credits = v.content.total_credits();
    Ok(v)
}

async fn get_version(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<CurriculumVersion, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load_version(&mut conn, tenant, id).await
}

const C_COLUMNS: &str = "c.id, c.institution_id, c.programme_id, c.code, c.title, c.status, \
    c.status_reason, c.published_at, c.retired_at, c.version, c.created_at, c.updated_at, \
    (SELECT v.id FROM institution.curriculum_version v WHERE v.curriculum_id = c.id \
       AND v.status = 'published') AS current_version_id, \
    (SELECT v.id FROM institution.curriculum_version v WHERE v.curriculum_id = c.id \
       AND v.status IN ('draft', 'in_review')) AS open_version_id";

fn curriculum_from_row(r: &PgRow) -> Result<Curriculum, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(Curriculum {
        id: CurriculumId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        programme_id: ProgrammeId(r.try_get("programme_id")?),
        code: r.try_get("code")?,
        title: r.try_get("title")?,
        status: parse_status(&status)?,
        current_version_id: r
            .try_get::<Option<Uuid>, _>("current_version_id")?
            .map(CurriculumVersionId),
        open_version_id: r
            .try_get::<Option<Uuid>, _>("open_version_id")?
            .map(CurriculumVersionId),
        status_reason: r.try_get("status_reason")?,
        published_at: r.try_get("published_at")?,
        retired_at: r.try_get("retired_at")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

pub(crate) async fn load_curriculum(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<Curriculum, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {C_COLUMNS} FROM institution.curriculum c WHERE c.id = $1 AND c.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: CURRICULUM })?;
    curriculum_from_row(&row).map_err(map_db_error)
}

async fn get_curriculum(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: Uuid,
) -> Result<Curriculum, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load_curriculum(&mut conn, tenant, id).await
}

async fn write_components(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    version_id: Uuid,
    components: &[CurriculumComponent],
) -> Result<(), KernelError> {
    for (i, c) in components.iter().enumerate() {
        sqlx::query(
            "INSERT INTO institution.curriculum_component (tenant_id, curriculum_version_id, \
             term_no, slot_code, title, category, credits, lecture_hours, tutorial_hours, \
             practical_hours, basket_code, course_code, sort_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(version_id)
        .bind(c.term_no)
        .bind(c.slot_code.trim())
        .bind(c.title.trim())
        .bind(c.category.as_str())
        .bind(c.credits)
        .bind(c.lecture_hours)
        .bind(c.tutorial_hours)
        .bind(c.practical_hours)
        .bind(c.basket_code.as_deref().map(str::trim))
        .bind(c.course_code.as_deref().map(str::trim))
        .bind(i32::try_from(i).unwrap_or(i32::MAX))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

fn awards_json(c: &CurriculumVersionContent) -> Result<serde_json::Value, KernelError> {
    serde_json::to_value(&c.exit_awards).map_err(KernelError::internal)
}

#[allow(clippy::too_many_arguments)]
async fn insert_version(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    curriculum_id: Uuid,
    programme_id: Uuid,
    institution_id: InstitutionId,
    supersedes: Option<Uuid>,
    c: &CurriculumVersionContent,
) -> Result<Uuid, KernelError> {
    let version_no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version_no), 0) + 1 FROM institution.curriculum_version WHERE curriculum_id = $1",
    )
    .bind(curriculum_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let id = CurriculumVersionId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.curriculum_version (id, tenant_id, institution_id, curriculum_id, \
         programme_id, version_no, supersedes_version_id, change_summary, effective_from, \
         applies_from_cohort, term_structure, term_count, credit_framework, min_total_credits, \
         min_credits_per_term, max_credits_per_term, nep_aligned, nheqf_level, \
         multiple_entry_exit, abc_enabled, exit_awards) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
         $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)",
    )
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(curriculum_id)
    .bind(programme_id)
    .bind(version_no)
    .bind(supersedes)
    .bind(c.change_summary.as_deref().map(str::trim))
    .bind(c.effective_from)
    .bind(c.applies_from_cohort.as_deref().map(str::trim))
    .bind(c.term_structure.as_str())
    .bind(c.term_count)
    .bind(c.credit_framework.as_str())
    .bind(c.min_total_credits)
    .bind(c.min_credits_per_term)
    .bind(c.max_credits_per_term)
    .bind(c.nep_aligned)
    .bind(c.nheqf_level)
    .bind(c.multiple_entry_exit)
    .bind(c.abc_enabled)
    .bind(awards_json(c)?)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    write_components(conn, scope, id, &c.components).await?;
    Ok(id)
}

/// The published version of a curriculum, if any.
async fn published_version_id(
    conn: &mut PgConnection,
    curriculum_id: Uuid,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM institution.curriculum_version WHERE curriculum_id = $1 AND status = 'published'",
    )
    .bind(curriculum_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Publication coherence of a version (L4-B-023-BH-002, L4-B-024-BH-007): the programme is
/// published, the credit rules hold, and the successor still builds on the currently published
/// version (a stale proposal is refused).
pub(crate) async fn validate_publication(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
) -> Result<(), KernelError> {
    let v = load_version(conn, scope, id).await?;
    let c = load_curriculum(conn, scope, v.curriculum_id.as_uuid()).await?;
    if c.status == PublicationStatus::Retired {
        return Err(KernelError::Conflict("the curriculum is retired".into()));
    }
    let p = programme::load(conn, scope, v.programme_id.as_uuid()).await?;
    if p.status != PublicationStatus::Published {
        return Err(KernelError::Conflict(format!(
            "programme {} is {}; publish it before its curriculum",
            p.code, p.status
        )));
    }
    let current = published_version_id(conn, v.curriculum_id.as_uuid()).await?;
    if current != v.supersedes_version_id.map(|s| s.as_uuid()) {
        return Err(KernelError::Conflict(
            "stale revision: the published version changed since this draft was created".into(),
        ));
    }
    if let Some(prev) = current {
        let prev = load_version(conn, scope, prev).await?;
        if v.content.effective_from < prev.content.effective_from {
            return Err(KernelError::Validation(vec![violation(
                "effectiveFrom",
                "before-predecessor",
                "a successor cannot take effect before its predecessor",
            )]));
        }
    }
    v.content.validate()?;
    finish(v.content.publication_rules(p.duration_terms))
}

pub(crate) async fn finalize_publication(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: Uuid,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let v = load_version(conn, scope, id).await?;
    ensure("curriculum version", v.status, PublicationStatus::Published)?;
    let curriculum_id = v.curriculum_id.as_uuid();
    let superseded = supersede_current(
        conn,
        scope,
        "curriculum_version",
        "curriculum_id",
        curriculum_id,
        id,
    )
    .await?;
    let new_version = move_status(
        conn,
        scope,
        "curriculum_version",
        id,
        PublicationStatus::Published,
        reason,
        None,
    )
    .await?;
    // The curriculum container becomes published with its first published version.
    sqlx::query(
        "UPDATE institution.curriculum SET status = 'published', published_at = COALESCE(published_at, now()), \
         version = version + 1, updated_at = now() WHERE id = $1 AND status = 'draft'",
    )
    .bind(curriculum_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let data = CurriculumVersionPublishedV1 {
        curriculum_id,
        curriculum_version_id: id,
        programme_id: v.programme_id.as_uuid(),
        institution_id: v.institution_id.as_uuid(),
        version_no: v.version_no,
        supersedes_version_id: superseded,
        effective_from: v.content.effective_from,
        total_credits: Some(format!("{}", v.total_credits)),
        process_instance_id: v.process_instance_id.clone(),
    };
    publish::event(
        conn,
        scope,
        CURRICULUM_VERSION_PUBLISHED,
        CURRICULUM_VERSION_PUBLISHED_SCHEMA,
        id.to_string(),
        &data,
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "institution.curriculum_version.publish",
        VERSION,
        id.to_string(),
        serde_json::json!({
            "fromStatus": v.status, "toStatus": "published", "version": new_version,
            "versionNo": v.version_no, "supersedesVersionId": superseded,
            "processInstanceId": v.process_instance_id, "reason": reason,
            "totalCredits": v.total_credits,
        }),
        actor,
    )
    .await
}

fn ok<T: serde::Serialize>(body: &T, version: Version) -> Response {
    with_etag(StatusCode::OK, body, version)
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

/// Create a curriculum for a programme with its first draft version (`curriculum.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/programmes/{programmeId}/curricula",
    operation_id = "curriculum_create",
    tag = "curriculum",
    params(
        ("programmeId" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateCurriculum,
    responses(
        (status = 201, body = Curriculum, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(programme_id): Path<ProgrammeId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateCurriculum>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    cmd.validate()?;
    let pid = programme_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "programme", "programme", pid).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = create_in(&mut tx, &scope, inst, pid, &cmd, &actor(ctx)).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &c, c.version))
}

/// Creates a curriculum with its first draft version inside the caller's transaction (the API
/// handler and the Part 2 · S4-import curriculum importer both use it).
pub async fn create_in(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    inst: InstitutionId,
    pid: Uuid,
    cmd: &CreateCurriculum,
    actor: &Actor,
) -> Result<Curriculum, KernelError> {
    cmd.validate()?;
    crate::shared::guard::lock_institution_for_change(&mut *conn, scope, inst).await?;
    let p = programme::load(&mut *conn, scope, pid).await?;
    if p.status == PublicationStatus::Retired {
        return Err(KernelError::Conflict("the programme is retired".into()));
    }
    let cid = CurriculumId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.curriculum (id, tenant_id, institution_id, programme_id, code, title) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(cid)
    .bind(scope.tenant_id.as_uuid())
    .bind(inst.as_uuid())
    .bind(pid)
    .bind(cmd.code.trim())
    .bind(cmd.title.trim())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let vid = insert_version(
        &mut *conn,
        scope,
        cid,
        pid,
        inst,
        None,
        &cmd.initial_version,
    )
    .await?;
    publish::audit(
        &mut *conn,
        scope,
        "institution.curriculum.create",
        CURRICULUM,
        cid.to_string(),
        serde_json::json!({"code": cmd.code.trim(), "programmeId": pid, "initialVersionId": vid}),
        actor,
    )
    .await?;
    let c = load_curriculum(&mut *conn, scope, cid).await?;
    Ok(c)
}

/// List the curricula of a programme (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/programmes/{programmeId}/curricula",
    operation_id = "curriculum_list",
    tag = "curriculum",
    params(("programmeId" = String, Path, format = Uuid), PublicationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CurriculumList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(programme_id): Path<ProgrammeId>,
    Query(q): Query<PublicationQuery>,
) -> ApiResult<Json<CurriculumList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let pid = programme_id.as_uuid();
    institution_of(&st.pool, &ctx.scope, "programme", "programme", pid).await?;
    let rows = sqlx::query(&format!(
        "SELECT {C_COLUMNS} FROM institution.curriculum c WHERE c.tenant_id = $1 \
         AND c.programme_id = $2 AND ($3::text IS NULL OR c.status = $3) ORDER BY c.code"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(pid)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(curriculum_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    Ok(Json(CurriculumList { items }))
}

/// Get one curriculum (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/curricula/{curriculumId}",
    operation_id = "curriculum_get",
    tag = "curriculum",
    params(("curriculumId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Curriculum, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(curriculum_id): Path<CurriculumId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let c = get_curriculum(&st.pool, &ctx.scope, curriculum_id.as_uuid()).await?;
    Ok(ok(&c, c.version))
}

/// Version history of a curriculum, newest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/curricula/{curriculumId}/versions",
    operation_id = "curriculum_version_list",
    tag = "curriculum",
    params(("curriculumId" = String, Path, format = Uuid), PublicationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CurriculumVersionList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn versions(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(curriculum_id): Path<CurriculumId>,
    Query(q): Query<PublicationQuery>,
) -> ApiResult<Json<CurriculumVersionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let cid = curriculum_id.as_uuid();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_curriculum(&mut conn, &ctx.scope, cid).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.curriculum_version WHERE tenant_id = $1 AND curriculum_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY version_no DESC",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(cid)
    .bind(q.status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        items.push(load_version(&mut conn, &ctx.scope, id).await?);
    }
    Ok(Json(CurriculumVersionList { items }))
}

/// Get one curriculum version with its slots (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/curriculum-versions/{versionId}",
    operation_id = "curriculum_version_get",
    tag = "curriculum",
    params(("versionId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CurriculumVersion, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_version_one(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(version_id): Path<CurriculumVersionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let v = get_version(&st.pool, &ctx.scope, version_id.as_uuid()).await?;
    Ok(ok(&v, v.version))
}

/// Replace the content of a draft version (`curriculum.manage`). Published, superseded and
/// retired versions are immutable (409).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/curriculum-versions/{versionId}",
    operation_id = "curriculum_version_update",
    tag = "curriculum",
    params(
        ("versionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the version being modified"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CurriculumVersionContent,
    responses(
        (status = 200, body = CurriculumVersion, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update_version(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(version_id): Path<CurriculumVersionId>,
    headers: HeaderMap,
    Json(c): Json<CurriculumVersionContent>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    c.validate()?;
    let id = version_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "curriculum_version", VERSION, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "curriculum_version", VERSION, id, true).await?;
    expect_version(expected, version)?;
    ensure_editable("curriculum version", status)?;
    sqlx::query(
        "UPDATE institution.curriculum_version SET change_summary = $1, effective_from = $2, \
         applies_from_cohort = $3, term_structure = $4, term_count = $5, credit_framework = $6, \
         min_total_credits = $7, min_credits_per_term = $8, max_credits_per_term = $9, \
         nep_aligned = $10, nheqf_level = $11, multiple_entry_exit = $12, abc_enabled = $13, \
         exit_awards = $14, version = version + 1, updated_at = now() WHERE id = $15 AND tenant_id = $16",
    )
    .bind(c.change_summary.as_deref().map(str::trim))
    .bind(c.effective_from)
    .bind(c.applies_from_cohort.as_deref().map(str::trim))
    .bind(c.term_structure.as_str())
    .bind(c.term_count)
    .bind(c.credit_framework.as_str())
    .bind(c.min_total_credits)
    .bind(c.min_credits_per_term)
    .bind(c.max_credits_per_term)
    .bind(c.nep_aligned)
    .bind(c.nheqf_level)
    .bind(c.multiple_entry_exit)
    .bind(c.abc_enabled)
    .bind(awards_json(&c)?)
    .bind(id)
    .bind(scope.tenant_id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query("DELETE FROM institution.curriculum_component WHERE curriculum_version_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    write_components(&mut tx, &scope, id, &c.components).await?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.curriculum_version.update",
        VERSION,
        id.to_string(),
        serde_json::json!({"fromVersion": expected.0, "slotCount": c.components.len()}),
        &actor(ctx),
    )
    .await?;
    let v = load_version(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&v, v.version))
}

/// Submit a draft version for publication (`curriculum.publish`; WorkflowPort
/// `CURRICULUM_PUBLICATION`). Auto-approval publishes it and supersedes the predecessor.
#[utoipa::path(
    post,
    path = "/api/v1/institution/curriculum-versions/{versionId}/publish",
    operation_id = "curriculum_version_publish",
    tag = "curriculum",
    params(
        ("versionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = CurriculumVersion, headers(("ETag" = String))),
        (status = 409, description = "Not a draft, programme not published, or stale revision", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Credit / NEP rules failed", body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn publish_version(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(version_id): Path<CurriculumVersionId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let id = version_id.as_uuid();
    publication::submit(
        &st,
        &ctx.scope,
        Kind::CurriculumVersion,
        id,
        expected,
        &actor(ctx),
    )
    .await?;
    let v = get_version(&st.pool, &ctx.scope, id).await?;
    Ok(ok(&v, v.version))
}

/// Decide an `in_review` version (`curriculum.publish`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/curriculum-versions/{versionId}/decision",
    operation_id = "curriculum_version_decision",
    tag = "curriculum",
    params(
        ("versionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses(
        (status = 200, body = CurriculumVersion, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decide_version(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(version_id): Path<CurriculumVersionId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let (d, reason) = DecisionBody::parse(&raw)?;
    let id = version_id.as_uuid();
    publication::decide(
        &st.pool,
        &ctx.scope,
        Kind::CurriculumVersion,
        id,
        expected,
        d,
        reason,
        &actor(ctx),
    )
    .await?;
    let v = get_version(&st.pool, &ctx.scope, id).await?;
    Ok(ok(&v, v.version))
}

/// Revise the published curriculum: creates the successor draft version linked to the
/// published predecessor (`curriculum.manage`). The predecessor stays authoritative until the
/// successor is published (L3-B-024).
#[utoipa::path(
    post,
    path = "/api/v1/institution/curricula/{curriculumId}/revisions",
    operation_id = "curriculum_revise",
    tag = "curriculum",
    params(
        ("curriculumId" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReviseCurriculum,
    responses(
        (status = 201, body = CurriculumVersion, headers(("ETag" = String))),
        (status = 409, description = "No published version, or a draft is already open", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revise(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(curriculum_id): Path<CurriculumId>,
    headers: HeaderMap,
    Json(cmd): Json<ReviseCurriculum>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumManage)?;
    require_idempotency_key(&headers)?;
    let mut v = vec![];
    required_text("changeSummary", &cmd.change_summary, 2000, &mut v);
    finish(v)?;
    let cid = curriculum_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "curriculum", CURRICULUM, cid).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    // Serialise revisions of one curriculum.
    let (cstatus, _) = head(&mut tx, &scope, "curriculum", CURRICULUM, cid, true).await?;
    if cstatus == PublicationStatus::Retired {
        return Err(KernelError::Conflict("the curriculum is retired".into()).into());
    }
    let Some(prev_id) = published_version_id(&mut tx, cid).await? else {
        return Err(KernelError::Conflict(
            "there is no published version to revise; edit the open draft instead".into(),
        )
        .into());
    };
    let prev = load_version(&mut tx, &scope, prev_id).await?;
    if cmd.effective_from < prev.content.effective_from {
        return Err(KernelError::Validation(vec![violation(
            "effectiveFrom",
            "before-predecessor",
            "a successor cannot take effect before its predecessor",
        )])
        .into());
    }
    let mut content = prev.content.clone();
    content.effective_from = cmd.effective_from;
    content.change_summary = Some(cmd.change_summary.trim().to_owned());
    content.applies_from_cohort = cmd
        .applies_from_cohort
        .clone()
        .or(content.applies_from_cohort);
    let vid = insert_version(
        &mut tx,
        &scope,
        cid,
        prev.programme_id.as_uuid(),
        inst,
        Some(prev_id),
        &content,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.curriculum.revise",
        CURRICULUM,
        cid.to_string(),
        serde_json::json!({"supersedesVersionId": prev_id, "successorVersionId": vid,
            "changeSummary": cmd.change_summary.trim()}),
        &actor(ctx),
    )
    .await?;
    let created = load_version(&mut tx, &scope, vid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &created, created.version))
}

/// Retire one published version (`curriculum.publish`, reason required).
#[utoipa::path(
    post,
    path = "/api/v1/institution/curriculum-versions/{versionId}/retire",
    operation_id = "curriculum_version_retire",
    tag = "curriculum",
    params(
        ("versionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the current version"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReasonBody,
    responses(
        (status = 200, body = CurriculumVersion, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retire_version(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(version_id): Path<CurriculumVersionId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let reason = body::<ReasonBody>(&raw)?.validated(true)?;
    let id = version_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "curriculum_version", VERSION, id).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "curriculum_version", VERSION, id, true).await?;
    expect_version(expected, version)?;
    ensure("curriculum version", status, PublicationStatus::Retired)?;
    let v = move_status(
        &mut tx,
        &scope,
        "curriculum_version",
        id,
        PublicationStatus::Retired,
        reason.as_deref(),
        None,
    )
    .await?;
    Transition {
        event_type: CURRICULUM_VERSION_RETIRED,
        dataschema: CURRICULUM_VERSION_RETIRED_SCHEMA,
        audit_action: "institution.curriculum_version.retire",
        target_type: VERSION,
        aggregate_id: id,
        from: status.as_str(),
        to: PublicationStatus::Retired.as_str(),
        version: v,
        reason,
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, &actor(ctx))
    .await?;
    let out = load_version(&mut tx, &scope, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&out, out.version))
}

/// Retire a curriculum (L3-B-025, `curriculum.publish`, reason required): the curriculum and,
/// as its explicit governed cascade, its published version become `retired`; history stays.
/// Publishes the canonical `CurriculumRetired` (`curriculum.retired`) only (amendment
/// 2026-08-24). Refused while a draft / in-review version is open.
#[utoipa::path(
    post,
    path = "/api/v1/institution/curricula/{curriculumId}/retire",
    operation_id = "curriculum_retire",
    tag = "curriculum",
    params(
        ("curriculumId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the curriculum"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReasonBody,
    responses(
        (status = 200, body = Curriculum, headers(("ETag" = String))),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(curriculum_id): Path<CurriculumId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CurriculumPublish)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(res) => return Ok(res),
    };
    let reason = body::<ReasonBody>(&raw)?.validated(true)?;
    let cid = curriculum_id.as_uuid();
    let inst = institution_of(&st.pool, &ctx.scope, "curriculum", CURRICULUM, cid).await?;
    let scope = institution_scope(ctx, inst);
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let (status, version) = head(&mut tx, &scope, "curriculum", CURRICULUM, cid, true).await?;
    expect_version(expected, version)?;
    super::domain::ensure_catalogue("curriculum", status, PublicationStatus::Retired)?;
    let c = load_curriculum(&mut tx, &scope, cid).await?;
    if c.open_version_id.is_some() {
        return Err(KernelError::Conflict(
            "a draft or in-review version is open; decide or discard it first".into(),
        )
        .into());
    }
    let mut cascaded = None;
    if let Some(vid) = c.current_version_id {
        move_status(
            &mut tx,
            &scope,
            "curriculum_version",
            vid.as_uuid(),
            PublicationStatus::Retired,
            reason.as_deref(),
            None,
        )
        .await?;
        cascaded = Some(vid);
    }
    let v = move_status(
        &mut tx,
        &scope,
        "curriculum",
        cid,
        PublicationStatus::Retired,
        reason.as_deref(),
        None,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.curriculum.retire_cascade",
        CURRICULUM,
        cid.to_string(),
        serde_json::json!({"retiredVersionIds": cascaded.into_iter().collect::<Vec<_>>()}),
        &actor(ctx),
    )
    .await?;
    Transition {
        event_type: CURRICULUM_RETIRED,
        dataschema: CURRICULUM_RETIRED_SCHEMA,
        audit_action: "institution.curriculum.retire",
        target_type: CURRICULUM,
        aggregate_id: cid,
        from: status.as_str(),
        to: PublicationStatus::Retired.as_str(),
        version: v,
        reason,
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, &actor(ctx))
    .await?;
    let out = load_curriculum(&mut tx, &scope, cid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(ok(&out, out.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(
        term: i32,
        code: &str,
        credits: f64,
        category: ComponentCategory,
    ) -> CurriculumComponent {
        CurriculumComponent {
            term_no: term,
            slot_code: code.into(),
            title: code.into(),
            category,
            credits,
            lecture_hours: 3,
            tutorial_hours: 1,
            practical_hours: 0,
            basket_code: None,
            course_code: None,
        }
    }

    pub(super) fn content() -> CurriculumVersionContent {
        CurriculumVersionContent {
            effective_from: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap_or_default(),
            applies_from_cohort: Some("2026-27".into()),
            change_summary: None,
            term_structure: TermStructure::Semester,
            term_count: 2,
            credit_framework: CreditFramework::Nep2020,
            min_total_credits: Some(40.0),
            min_credits_per_term: Some(18.0),
            max_credits_per_term: Some(26.0),
            nep_aligned: true,
            nheqf_level: Some(4.5),
            multiple_entry_exit: true,
            abc_enabled: true,
            exit_awards: vec![ExitAward {
                after_terms: 1,
                award: "Certificate".into(),
                min_credits: 20.0,
            }],
            components: vec![
                slot(1, "CS101", 12.0, ComponentCategory::MajorCore),
                slot(1, "MA101", 8.5, ComponentCategory::Minor),
                slot(2, "CS102", 12.0, ComponentCategory::MajorCore),
                slot(2, "SE101", 8.0, ComponentCategory::SkillEnhancement),
            ],
        }
    }

    #[test]
    fn credit_rules() {
        let c = content();
        assert!(c.validate().is_ok());
        assert!((c.total_credits() - 40.5).abs() < f64::EPSILON);
        assert!(
            c.publication_rules(2).is_empty(),
            "{:?}",
            c.publication_rules(2)
        );
        // Programme duration mismatch.
        assert_eq!(c.publication_rules(4)[0].code, "programme-mismatch");
        // Per-term limits, total minimum, empty term, unreachable exit, NEP completeness.
        let mut bad = content();
        bad.components.retain(|s| s.term_no == 1);
        bad.min_total_credits = Some(160.0);
        bad.max_credits_per_term = Some(20.0);
        bad.exit_awards[0].min_credits = 30.0;
        bad.nheqf_level = None;
        let codes: Vec<_> = bad
            .publication_rules(2)
            .into_iter()
            .map(|v| v.code)
            .collect();
        for expect in [
            "empty-term",
            "term-credits",
            "total-credits",
            "unreachable",
            "nep",
        ] {
            assert!(
                codes.iter().any(|c| c == expect),
                "{expect} missing in {codes:?}"
            );
        }
    }

    #[test]
    fn shape_rules() {
        let mut c = content();
        c.components
            .push(slot(3, "CS101", 0.25, ComponentCategory::MajorCore));
        c.exit_awards.push(ExitAward {
            after_terms: 1,
            award: "Dup".into(),
            min_credits: 10.0,
        });
        c.min_credits_per_term = Some(30.0);
        let Err(KernelError::Validation(v)) = c.validate() else {
            panic!("expected violations")
        };
        let codes: Vec<_> = v.iter().map(|x| x.code.as_str()).collect();
        for expect in ["duplicate", "range", "format", "order"] {
            assert!(codes.contains(&expect), "{expect} missing in {codes:?}");
        }
        let mut no_mee = content();
        no_mee.multiple_entry_exit = false;
        assert!(no_mee.validate().is_err());
        for t in [
            TermStructure::Semester,
            TermStructure::Trimester,
            TermStructure::Annual,
        ] {
            assert_eq!(TermStructure::parse(t.as_str()), Some(t));
        }
        assert_eq!(
            ComponentCategory::parse("open_elective"),
            Some(ComponentCategory::OpenElective)
        );
    }
}
