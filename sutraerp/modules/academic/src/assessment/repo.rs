//! Internal assessment and coursework (L3-D-051…057): blueprint → windows/assignments → student
//! submissions → marking → moderation → finalised internal assessment → correction / archive.

use super::domain::{self, BlueprintComponent, BlueprintPolicy, ComponentMark, Score};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::faculty::require_teaching;
use crate::shared::guard::require_institution_writable;
use crate::shared::{publish, AcademicState, Actor};
use crate::timetable::kit::{
    self, check_version, illegal, invalid, required_text, SectionCtx, StatusEvent,
};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::delivery::*;
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::ports::RosterEntry;
use sutra_contracts::academic::status::{
    AssessmentBlueprintStatus, AssessmentKind, AssessmentSubmissionStatus, AssessmentWindowStatus,
    ChangeRequestStatus, InternalAssessmentStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// ------------------------------------------------------------------ DTOs

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentBlueprint {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub title: String,
    pub total_marks: f64,
    pub components: Vec<BlueprintComponent>,
    pub policy: BlueprintPolicy,
    pub status: AssessmentBlueprintStatus,
    pub status_reason: Option<String>,
    pub supersedes_id: Option<Uuid>,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AssessmentBlueprintList {
    pub items: Vec<AssessmentBlueprint>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAssessmentBlueprint {
    pub title: String,
    pub total_marks: f64,
    pub components: Vec<BlueprintComponent>,
    pub policy: Option<BlueprintPolicy>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAssessmentBlueprint {
    pub title: Option<String>,
    pub total_marks: Option<f64>,
    pub components: Option<Vec<BlueprintComponent>>,
    pub policy: Option<BlueprintPolicy>,
}

/// An assessment window; coursework is kind `assignment` (contract §5.3).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentWindow {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub blueprint_id: Uuid,
    pub component_key: String,
    pub title: String,
    pub kind: AssessmentKind,
    pub instructions: Option<String>,
    pub opens_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub extended_due_at: Option<DateTime<Utc>>,
    pub max_marks: f64,
    pub status: AssessmentWindowStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AssessmentWindowList {
    pub items: Vec<AssessmentWindow>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAssessmentWindow {
    pub blueprint_id: Uuid,
    pub component_key: String,
    pub title: String,
    /// Defaults to the component's kind.
    pub kind: Option<AssessmentKind>,
    pub instructions: Option<String>,
    pub opens_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub max_marks: f64,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtendWindow {
    pub due_at: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentSubmission {
    /// `null` for a seated student with nothing recorded yet (status `pending`).
    pub id: Option<Uuid>,
    pub window_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub status: AssessmentSubmissionStatus,
    pub content: Option<String>,
    pub attachment_url: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub marks: Option<f64>,
    pub feedback: Option<String>,
    pub marked_at: Option<DateTime<Utc>>,
    pub moderated_marks: Option<f64>,
    pub version: Option<i64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmissionRow {
    id: Uuid,
    window_id: Uuid,
    student_id: Uuid,
    student_number: String,
    display_name: String,
    subject_ref: Option<String>,
    status: AssessmentSubmissionStatus,
    content: Option<String>,
    attachment_url: Option<String>,
    submitted_at: Option<DateTime<Utc>>,
    marks: Option<f64>,
    feedback: Option<String>,
    marked_at: Option<DateTime<Utc>>,
    moderated_marks: Option<f64>,
    version: i64,
}

impl SubmissionRow {
    fn view(&self) -> AssessmentSubmission {
        AssessmentSubmission {
            id: Some(self.id),
            window_id: self.window_id,
            student_id: self.student_id,
            student_number: self.student_number.clone(),
            display_name: self.display_name.clone(),
            status: self.status,
            content: self.content.clone(),
            attachment_url: self.attachment_url.clone(),
            submitted_at: self.submitted_at,
            marks: self.marks,
            feedback: self.feedback.clone(),
            marked_at: self.marked_at,
            moderated_marks: self.moderated_marks,
            version: Some(self.version),
        }
    }
    /// Marks that count (moderated outcome wins).
    fn effective(&self) -> Option<f64> {
        self.moderated_marks.or(self.marks)
    }
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AssessmentSubmissionList {
    pub items: Vec<AssessmentSubmission>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkSubmission {
    pub marks: f64,
    pub feedback: Option<String>,
    /// Required when changing marks already recorded (kept in the mark history).
    pub reason: Option<String>,
}

/// Faculty record for a student without a submission (offline test, absence).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecordOutcome {
    pub student_id: Uuid,
    pub marks: Option<f64>,
    #[serde(default)]
    pub absent: bool,
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSubmission {
    pub content: Option<String>,
    pub attachment_url: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentModeration {
    pub id: Uuid,
    pub window_id: Uuid,
    pub submission_id: Uuid,
    pub original_marks: f64,
    pub proposed_marks: f64,
    pub reason: String,
    pub evidence: Option<String>,
    pub status: ChangeRequestStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AssessmentModerationList {
    pub items: Vec<AssessmentModeration>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewModeration {
    pub submission_id: Uuid,
    pub proposed_marks: f64,
    pub reason: String,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InternalAssessmentResult {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub blueprint_id: Uuid,
    pub academic_term_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub component_marks: Vec<ComponentMark>,
    pub marks_obtained: f64,
    pub max_marks: f64,
    pub status: InternalAssessmentStatus,
    pub status_reason: Option<String>,
    pub result_version: i32,
    pub supersedes_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing, default)]
    subject_ref: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct InternalAssessmentResultList {
    pub items: Vec<InternalAssessmentResult>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentCorrection {
    pub id: Uuid,
    pub result_id: Uuid,
    pub corrected_result_id: Option<Uuid>,
    pub from_marks: f64,
    pub to_marks: f64,
    pub reason: String,
    pub evidence: Option<String>,
    pub status: ChangeRequestStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AssessmentCorrectionList {
    pub items: Vec<AssessmentCorrection>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAssessmentCorrection {
    pub marks_obtained: f64,
    pub reason: String,
    pub evidence: Option<String>,
}

/// One coursework item as the student sees it (contract §5.3).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyAssignment {
    pub id: Uuid,
    pub section_id: Uuid,
    pub offering_code: String,
    pub course_title: String,
    pub title: String,
    pub kind: AssessmentKind,
    pub instructions: Option<String>,
    pub opens_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub extended_due_at: Option<DateTime<Utc>>,
    pub max_marks: f64,
    pub status: AssessmentWindowStatus,
    pub submission: Option<MySubmission>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MySubmission {
    pub status: AssessmentSubmissionStatus,
    pub submitted_at: Option<DateTime<Utc>>,
    /// Moderated marks when moderated, else the recorded marks.
    pub marks: Option<f64>,
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MyAssignments {
    pub items: Vec<MyAssignment>,
}

const BP: &str = "assessment_blueprint";
const WN: &str = "assessment_window";
const SB: &str = "assessment_submission";
const MO: &str = "assessment_moderation";
const RS: &str = "internal_assessment_result";
const CO: &str = "assessment_correction";

// ------------------------------------------------------------------ helpers

async fn roster(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
) -> Result<Vec<RosterEntry>, KernelError> {
    state
        .offerings
        .roster(
            &ExecutionScope::tenant(tenant),
            OfferingSectionId(section),
            true,
        )
        .await
}

/// Authorises a teaching command on a section and returns its writable context.
async fn teaching_ctx(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    manage: Permission,
) -> Result<SectionCtx, KernelError> {
    require_teaching(state, auth, OfferingSectionId(section), manage).await?;
    let c = kit::section_ctx(state, auth, section).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    Ok(c)
}

fn status_event<'a>(
    c: &'a SectionCtx,
    id: Uuid,
    from: Option<&'a str>,
    to: &'a str,
    version: i64,
) -> StatusEvent<'a> {
    StatusEvent {
        aggregate_id: id,
        institution_id: c.institution_uuid(),
        offering_id: Some(c.offering.id.as_uuid()),
        section_id: Some(c.section.id.as_uuid()),
        from,
        to,
        version,
        ..Default::default()
    }
}

// ------------------------------------------------------------------ blueprints

pub async fn list_blueprints(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
) -> Result<Vec<AssessmentBlueprint>, KernelError> {
    kit::list_by(
        &state.pool,
        BP,
        tenant,
        "section_id",
        section,
        None,
        "t.created_at DESC",
        100,
    )
    .await
}

pub async fn get_blueprint(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AssessmentBlueprint, KernelError> {
    kit::fetch(&state.pool, BP, "assessment blueprint", tenant, id).await
}

pub async fn create_blueprint(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    cmd: NewAssessmentBlueprint,
    actor: &Actor,
) -> Result<AssessmentBlueprint, KernelError> {
    require_teaching(
        state,
        auth,
        OfferingSectionId(section),
        Permission::AssessmentManage,
    )
    .await?;
    let c = kit::deliverable_section(state, auth, section).await?;
    let title = required_text("title", Some(&cmd.title))?;
    if cmd.total_marks <= 0.0 {
        return Err(invalid(
            "totalMarks",
            "range",
            "totalMarks must be positive",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b: AssessmentBlueprint = kit::insert(&mut tx, BP, json!({
        "tenant_id": auth.scope.tenant_id.as_uuid(), "institution_id": c.institution_uuid(), "offering_id": c.offering.id.as_uuid(),
        "section_id": section, "title": title, "total_marks": cmd.total_marks, "components": cmd.components,
        "policy": cmd.policy.unwrap_or(BlueprintPolicy { moderation_required: false, allow_late_submission: true }),
    })).await?;
    kit::audit(
        &mut tx,
        &c.scope,
        BP,
        "create",
        b.id,
        json!({"sectionId": section}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(b)
}

pub async fn update_blueprint(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: UpdateAssessmentBlueprint,
    actor: &Actor,
) -> Result<AssessmentBlueprint, KernelError> {
    crate::shared::faculty::require_teaching_role(auth, Permission::AssessmentManage)?;
    let tenant = auth.scope.tenant_id;
    let cur = get_blueprint(state, tenant, id).await?;
    let c = teaching_ctx(state, auth, cur.section_id, Permission::AssessmentManage).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b: AssessmentBlueprint = kit::lock(&mut tx, BP, "assessment blueprint", tenant, id).await?;
    check_version(b.version, expected)?;
    if b.status != AssessmentBlueprintStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "a {} blueprint is immutable",
            b.status
        )));
    }
    let mut changes = serde_json::Map::new();
    if let Some(t) = cmd.title.as_deref() {
        changes.insert("title".into(), json!(required_text("title", Some(t))?));
    }
    if let Some(m) = cmd.total_marks {
        if m <= 0.0 {
            return Err(invalid(
                "totalMarks",
                "range",
                "totalMarks must be positive",
            ));
        }
        changes.insert("total_marks".into(), json!(m));
    }
    if let Some(cs) = cmd.components {
        changes.insert("components".into(), json!(cs));
    }
    if let Some(p) = cmd.policy {
        changes.insert("policy".into(), json!(p));
    }
    let out = kit::patch(&mut tx, BP, tenant, id, serde_json::Value::Object(changes)).await?;
    kit::audit(&mut tx, &c.scope, BP, "update", id, json!({}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Institutional publication decision (`assessment.manage`, L4-D-051-BH-005).
pub async fn publish_blueprint(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<AssessmentBlueprint, KernelError> {
    auth.require(Permission::AssessmentManage)?;
    let tenant = auth.scope.tenant_id;
    let cur = get_blueprint(state, tenant, id).await?;
    let c = kit::deliverable_section(state, auth, cur.section_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b: AssessmentBlueprint = kit::lock(&mut tx, BP, "assessment blueprint", tenant, id).await?;
    check_version(b.version, expected)?;
    if !domain::blueprint_can_transition(b.status, AssessmentBlueprintStatus::Published) {
        return Err(illegal(
            "assessment blueprint",
            b.status,
            AssessmentBlueprintStatus::Published,
        ));
    }
    let errors = domain::validate_blueprint(&b.components);
    if !errors.is_empty() {
        return Err(invalid("components", "blueprint", errors.join("; ")));
    }
    let prior: Option<Uuid> = sqlx::query_scalar("SELECT id FROM academic.assessment_blueprint WHERE section_id = $1 AND status = 'published' FOR UPDATE")
        .bind(b.section_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if let Some(p) = prior {
        let live: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.assessment_window WHERE blueprint_id = $1 AND status IN ('open', 'extended')")
            .bind(p)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        if live > 0 {
            return Err(KernelError::Conflict(
                "the current blueprint still has open assessment windows".into(),
            ));
        }
        kit::patch::<AssessmentBlueprint>(
            &mut tx,
            BP,
            tenant,
            p,
            json!({"status": "superseded", "status_reason": "superseded by a newer blueprint"}),
        )
        .await?;
    }
    let out: AssessmentBlueprint = kit::patch(&mut tx, BP, tenant, id, json!({"status": "published", "published_at": Utc::now(), "published_by": actor.id, "supersedes_id": prior})).await?;
    status_event(&c, id, Some("draft"), "published", out.version)
        .emit(
            &mut tx,
            &c.scope,
            (
                ASSESSMENT_BLUEPRINT_PUBLISHED,
                ASSESSMENT_BLUEPRINT_PUBLISHED_SCHEMA,
            ),
            actor,
        )
        .await?;
    kit::audit(
        &mut tx,
        &c.scope,
        BP,
        "publish",
        id,
        json!({"supersedes": prior}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ windows

pub async fn list_windows(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
    status: Option<&str>,
) -> Result<Vec<AssessmentWindow>, KernelError> {
    kit::list_by(
        &state.pool,
        WN,
        tenant,
        "section_id",
        section,
        status,
        "t.due_at, t.id",
        200,
    )
    .await
}

pub async fn get_window(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AssessmentWindow, KernelError> {
    kit::fetch(&state.pool, WN, "assessment window", tenant, id).await
}

pub async fn create_window(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    cmd: NewAssessmentWindow,
    actor: &Actor,
) -> Result<AssessmentWindow, KernelError> {
    require_teaching(
        state,
        auth,
        OfferingSectionId(section),
        Permission::AssessmentManage,
    )
    .await?;
    let c = kit::deliverable_section(state, auth, section).await?;
    let tenant = auth.scope.tenant_id;
    let title = required_text("title", Some(&cmd.title))?;
    let bp = get_blueprint(state, tenant, cmd.blueprint_id)
        .await
        .map_err(|_| invalid("blueprintId", "not_found", "no such blueprint"))?;
    if bp.section_id != section {
        return Err(invalid(
            "blueprintId",
            "section",
            "the blueprint belongs to another section",
        ));
    }
    if bp.status != AssessmentBlueprintStatus::Published {
        return Err(KernelError::Conflict(format!(
            "the blueprint is {}; windows need a published blueprint",
            bp.status
        )));
    }
    let comp = bp
        .components
        .iter()
        .find(|x| x.key == cmd.component_key)
        .ok_or_else(|| {
            invalid(
                "componentKey",
                "not_found",
                "the blueprint has no such component",
            )
        })?;
    if cmd.due_at <= cmd.opens_at {
        return Err(invalid("dueAt", "range", "dueAt must be after opensAt"));
    }
    if cmd.max_marks <= 0.0 {
        return Err(invalid("maxMarks", "range", "maxMarks must be positive"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let w: AssessmentWindow = kit::insert(&mut tx, WN, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "offering_id": c.offering.id.as_uuid(),
        "section_id": section, "blueprint_id": bp.id, "component_key": comp.key, "title": title,
        "kind": cmd.kind.unwrap_or(comp.kind).as_str(), "instructions": kit::opt_text(cmd.instructions.as_deref()),
        "opens_at": cmd.opens_at, "due_at": cmd.due_at, "max_marks": cmd.max_marks, "created_by": actor.id,
    })).await?;
    kit::audit(
        &mut tx,
        &c.scope,
        WN,
        "create",
        w.id,
        json!({"componentKey": comp.key}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(w)
}

/// Window commands: `open`, `close`, `cancel`, `extend`.
#[allow(clippy::too_many_arguments)]
pub async fn window_command(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: &str,
    reason: Option<String>,
    extend_to: Option<DateTime<Utc>>,
    actor: &Actor,
) -> Result<AssessmentWindow, KernelError> {
    use AssessmentWindowStatus as W;
    crate::shared::faculty::require_teaching_role(auth, Permission::AssessmentManage)?;
    let tenant = auth.scope.tenant_id;
    let cur = get_window(state, tenant, id).await?;
    if cmd == "extend" {
        // an extension is an exceptional change needing assessment authority (L4-D-052-BH-005)
        auth.require(Permission::AssessmentManage)?;
    }
    let c = teaching_ctx(state, auth, cur.section_id, Permission::AssessmentManage).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let w: AssessmentWindow = kit::lock(&mut tx, WN, "assessment window", tenant, id).await?;
    check_version(w.version, expected)?;
    let to = match cmd {
        "open" => W::Open,
        "close" => W::Closed,
        "cancel" => W::Cancelled,
        _ => W::Extended,
    };
    if !domain::window_can_transition(w.status, to) {
        return Err(illegal("assessment window", w.status, to));
    }
    let mut changes = json!({"status": to.as_str(), "status_reason": reason});
    match cmd {
        "open" => {
            crate::shared::guard::require_deliverable(c.offering.status, c.section.status)?;
            let bp = get_blueprint(state, tenant, w.blueprint_id).await?;
            if bp.status != AssessmentBlueprintStatus::Published {
                return Err(KernelError::Conflict(format!(
                    "the blueprint is {}; the window cannot open",
                    bp.status
                )));
            }
        }
        "cancel" => {
            required_text("reason", reason.as_deref())?;
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM academic.assessment_submission WHERE window_id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            if n > 0 {
                return Err(KernelError::Conflict(
                    "the window has submissions; close it instead".into(),
                ));
            }
        }
        "extend" => {
            required_text("reason", reason.as_deref())?;
            let due = extend_to
                .ok_or_else(|| invalid("dueAt", "required", "the new due date is required"))?;
            let current = w.extended_due_at.unwrap_or(w.due_at);
            if due <= current {
                return Err(invalid(
                    "dueAt",
                    "range",
                    "an extension moves the due date later",
                ));
            }
            if let Some(m) = changes.as_object_mut() {
                m.insert("extended_due_at".into(), json!(due));
            }
        }
        _ => {}
    }
    let out: AssessmentWindow = kit::patch(&mut tx, WN, tenant, id, changes).await?;
    let ev = match cmd {
        "open" => Some((ASSESSMENT_WINDOW_OPENED, ASSESSMENT_WINDOW_OPENED_SCHEMA)),
        "close" => Some((ASSESSMENT_WINDOW_CLOSED, ASSESSMENT_WINDOW_CLOSED_SCHEMA)),
        _ => None,
    };
    if let Some(ev) = ev {
        let mut e = status_event(
            &c,
            id,
            Some(w.status.as_str()),
            out.status.as_str(),
            out.version,
        );
        e.reason = Some(out.title.as_str());
        e.emit(&mut tx, &c.scope, ev, actor).await?;
    }
    kit::audit(
        &mut tx,
        &c.scope,
        WN,
        cmd,
        id,
        json!({"from": w.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ submissions + marking

async fn submission_rows<'e, E: sqlx::PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    window: Uuid,
) -> Result<Vec<SubmissionRow>, KernelError> {
    kit::list_by(
        ex,
        SB,
        tenant,
        "window_id",
        window,
        None,
        "t.student_number, t.id",
        5000,
    )
    .await
}

pub async fn list_submissions(
    state: &AcademicState,
    tenant: TenantId,
    window: &AssessmentWindow,
) -> Result<Vec<AssessmentSubmission>, KernelError> {
    let rows = submission_rows(&state.pool, tenant, window.id).await?;
    let mut out: Vec<AssessmentSubmission> = rows.iter().map(SubmissionRow::view).collect();
    for r in roster(state, tenant, window.section_id).await? {
        if !out.iter().any(|s| s.student_id == r.student_id.as_uuid()) {
            out.push(AssessmentSubmission {
                id: None,
                window_id: window.id,
                student_id: r.student_id.as_uuid(),
                student_number: r.student_number,
                display_name: r.display_name,
                status: AssessmentSubmissionStatus::Pending,
                content: None,
                attachment_url: None,
                submitted_at: None,
                marks: None,
                feedback: None,
                marked_at: None,
                moderated_marks: None,
                version: None,
            });
        }
    }
    out.sort_by(|a, b| a.student_number.cmp(&b.student_number));
    Ok(out)
}

/// Student submission (`academic.self`; the student comes from the session, L4-D-053).
pub async fn submit_own(
    state: &AcademicState,
    auth: &AuthContext,
    window_id: Uuid,
    cmd: NewSubmission,
    actor: &Actor,
) -> Result<MyAssignment, KernelError> {
    let me = crate::shared::me::me(state, auth).await?;
    let tenant = auth.scope.tenant_id;
    let w = get_window(state, tenant, window_id)
        .await
        .map_err(|_| KernelError::NotFound {
            entity: "assignment",
        })?;
    crate::shared::me::require_seated(state, auth, OfferingSectionId(w.section_id))
        .await
        .map_err(|_| KernelError::NotFound {
            entity: "assignment",
        })?;
    let c = kit::section_ctx(state, auth, w.section_id).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    if kit::opt_text(cmd.content.as_deref()).is_none()
        && kit::opt_text(cmd.attachment_url.as_deref()).is_none()
    {
        return Err(invalid(
            "content",
            "required",
            "submit some content or an attachment link",
        ));
    }
    let bp = get_blueprint(state, tenant, w.blueprint_id).await?;
    let roster = roster(state, tenant, w.section_id).await?;
    let entry = roster
        .iter()
        .find(|r| r.student_id == me.id)
        .ok_or(KernelError::NotFound {
            entity: "assignment",
        })?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let w: AssessmentWindow =
        kit::lock(&mut tx, WN, "assessment window", tenant, window_id).await?;
    if !domain::accepts_submissions(w.status) {
        return Err(KernelError::Conflict(format!(
            "the assignment is {}; submissions are not accepted",
            w.status
        )));
    }
    let now = Utc::now();
    if now < w.opens_at {
        return Err(KernelError::Conflict(
            "the assignment is not open for submissions yet".into(),
        ));
    }
    let deadline = w.extended_due_at.unwrap_or(w.due_at);
    let status = if now <= w.due_at {
        AssessmentSubmissionStatus::Submitted
    } else if now <= deadline && bp.policy.allow_late_submission {
        AssessmentSubmissionStatus::Late
    } else {
        return Err(KernelError::Conflict("the due date has passed".into()));
    };
    let row: SubmissionRow = kit::insert(&mut tx, SB, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "window_id": window_id, "section_id": w.section_id,
        "student_id": me.id.as_uuid(), "student_number": entry.student_number, "display_name": entry.display_name,
        "subject_ref": entry.subject_ref, "status": status.as_str(), "content": kit::opt_text(cmd.content.as_deref()),
        "attachment_url": kit::opt_text(cmd.attachment_url.as_deref()), "submitted_at": now,
    }))
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("you have already submitted this assignment".into()),
        other => other,
    })?;
    let mut e = status_event(&c, row.id, None, status.as_str(), row.version);
    e.student_id = Some(me.id.as_uuid());
    e.student_subject_ref = row.subject_ref.as_deref();
    e.emit(
        &mut tx,
        &c.scope,
        (
            ASSESSMENT_SUBMISSION_RECEIVED,
            ASSESSMENT_SUBMISSION_RECEIVED_SCHEMA,
        ),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &c.scope,
        SB,
        "submit",
        row.id,
        json!({"windowId": window_id, "status": status}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mine = my_assignments(state, auth).await?;
    mine.items
        .into_iter()
        .find(|a| a.id == window_id)
        .ok_or(KernelError::NotFound {
            entity: "assignment",
        })
}

pub async fn mark(
    state: &AcademicState,
    auth: &AuthContext,
    window_id: Uuid,
    submission_id: Uuid,
    expected: Version,
    cmd: MarkSubmission,
    actor: &Actor,
) -> Result<AssessmentSubmission, KernelError> {
    let tenant = auth.scope.tenant_id;
    crate::shared::faculty::require_teaching_role(auth, Permission::AssessmentManage)?;
    let w = get_window(state, tenant, window_id).await?;
    let c = teaching_ctx(state, auth, w.section_id, Permission::AssessmentManage).await?;
    if cmd.marks < 0.0 || cmd.marks > w.max_marks {
        return Err(invalid(
            "marks",
            "range",
            format!("marks are between 0 and {}", w.max_marks),
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let s: SubmissionRow = kit::lock(&mut tx, SB, "submission", tenant, submission_id).await?;
    if s.window_id != window_id {
        return Err(KernelError::NotFound {
            entity: "submission",
        });
    }
    check_version(s.version, expected)?;
    use AssessmentSubmissionStatus as S;
    let remark = s.marks.is_some();
    match s.status {
        S::Submitted | S::Late | S::Marked | S::Absent => {}
        other => {
            return Err(KernelError::Conflict(format!(
                "a {other} submission cannot be re-marked; use moderation or a correction"
            )))
        }
    }
    let reason = kit::opt_text(cmd.reason.as_deref());
    if remark && reason.is_none() {
        return Err(invalid(
            "reason",
            "required",
            "changing recorded marks needs a reason",
        ));
    }
    let out: SubmissionRow = kit::patch(&mut tx, SB, tenant, submission_id, json!({
        "status": "marked", "marks": cmd.marks, "feedback": kit::opt_text(cmd.feedback.as_deref()).or(s.feedback.clone()),
        "marked_by": actor.id, "marked_at": Utc::now(),
    })).await?;
    kit::append(&mut tx, "assessment_mark_history", json!({
        "tenant_id": tenant.as_uuid(), "submission_id": submission_id, "from_marks": s.marks, "to_marks": cmd.marks,
        "cause": if remark { "remarked" } else { "marked" }, "reason": reason, "actor": actor.id,
    })).await?;
    kit::audit(
        &mut tx,
        &c.scope,
        SB,
        if remark { "remark" } else { "mark" },
        submission_id,
        json!({"marks": cmd.marks}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out.view())
}

/// Faculty record for a student without a submission (offline assessment or absence).
pub async fn record_outcome(
    state: &AcademicState,
    auth: &AuthContext,
    window_id: Uuid,
    cmd: RecordOutcome,
    actor: &Actor,
) -> Result<AssessmentSubmission, KernelError> {
    let tenant = auth.scope.tenant_id;
    crate::shared::faculty::require_teaching_role(auth, Permission::AssessmentManage)?;
    let w = get_window(state, tenant, window_id).await?;
    let c = teaching_ctx(state, auth, w.section_id, Permission::AssessmentManage).await?;
    if matches!(
        w.status,
        AssessmentWindowStatus::Scheduled | AssessmentWindowStatus::Cancelled
    ) {
        return Err(KernelError::Conflict(format!(
            "outcomes are not recorded for a {} window",
            w.status
        )));
    }
    let (status, marks) = match (cmd.absent, cmd.marks) {
        (true, _) => (AssessmentSubmissionStatus::Absent, None),
        (false, Some(m)) if (0.0..=w.max_marks).contains(&m) => {
            (AssessmentSubmissionStatus::Marked, Some(m))
        }
        (false, Some(_)) => {
            return Err(invalid(
                "marks",
                "range",
                format!("marks are between 0 and {}", w.max_marks),
            ))
        }
        (false, None) => {
            return Err(invalid(
                "marks",
                "required",
                "give marks or mark the student absent",
            ))
        }
    };
    let entry = roster(state, tenant, w.section_id)
        .await?
        .into_iter()
        .find(|r| r.student_id.as_uuid() == cmd.student_id)
        .ok_or_else(|| {
            invalid(
                "studentId",
                "not_seated",
                "the student is not seated in this section",
            )
        })?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row: SubmissionRow = kit::insert(&mut tx, SB, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "window_id": window_id, "section_id": w.section_id,
        "student_id": cmd.student_id, "student_number": entry.student_number, "display_name": entry.display_name,
        "subject_ref": entry.subject_ref, "status": status.as_str(), "marks": marks,
        "feedback": kit::opt_text(cmd.feedback.as_deref()), "marked_by": actor.id, "marked_at": Utc::now(),
    }))
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("the student already has a submission for this window; mark it instead".into()),
        other => other,
    })?;
    kit::append(&mut tx, "assessment_mark_history", json!({
        "tenant_id": tenant.as_uuid(), "submission_id": row.id, "from_marks": null, "to_marks": marks,
        "cause": if cmd.absent { "absent" } else { "marked" }, "actor": actor.id,
    })).await?;
    kit::audit(
        &mut tx,
        &c.scope,
        SB,
        "record",
        row.id,
        json!({"status": status}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(row.view())
}

// ------------------------------------------------------------------ moderation

pub async fn list_moderations(
    state: &AcademicState,
    tenant: TenantId,
    window: Uuid,
) -> Result<Vec<AssessmentModeration>, KernelError> {
    kit::list_by(
        &state.pool,
        MO,
        tenant,
        "window_id",
        window,
        None,
        "t.created_at DESC",
        500,
    )
    .await
}

async fn apply_moderation(
    conn: &mut PgConnection,
    c: &SectionCtx,
    m: &AssessmentModeration,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AssessmentModeration, KernelError> {
    let tenant = c.scope.tenant_id;
    if !approve {
        let out = kit::patch(conn, MO, tenant, m.id, json!({"status": "rejected", "status_reason": reason, "decided_by": actor.id, "decided_at": Utc::now()})).await?;
        kit::audit(
            conn,
            &c.scope,
            MO,
            "reject",
            m.id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        return Ok(out);
    }
    let s: SubmissionRow = kit::lock(conn, SB, "submission", tenant, m.submission_id).await?;
    let ns: SubmissionRow = kit::patch(
        conn,
        SB,
        tenant,
        s.id,
        json!({"status": "moderated", "moderated_marks": m.proposed_marks}),
    )
    .await?;
    kit::append(conn, "assessment_mark_history", json!({
        "tenant_id": tenant.as_uuid(), "submission_id": s.id, "from_marks": s.effective(), "to_marks": m.proposed_marks,
        "cause": "moderated", "reason": m.reason, "actor": actor.id,
    })).await?;
    let out: AssessmentModeration = kit::patch(conn, MO, tenant, m.id, json!({"status": "applied", "status_reason": reason, "decided_by": actor.id, "decided_at": Utc::now()})).await?;
    let mut e = status_event(c, ns.id, Some(s.status.as_str()), "moderated", ns.version);
    e.student_id = Some(ns.student_id);
    e.student_subject_ref = ns.subject_ref.as_deref();
    e.process_instance_id = m.process_instance_id.as_deref();
    e.emit(
        conn,
        &c.scope,
        (
            ASSESSMENT_OUTCOME_MODERATED,
            ASSESSMENT_OUTCOME_MODERATED_SCHEMA,
        ),
        actor,
    )
    .await?;
    kit::audit(
        conn,
        &c.scope,
        MO,
        "apply",
        m.id,
        json!({"from": m.original_marks, "to": m.proposed_marks}),
        actor,
    )
    .await?;
    Ok(out)
}

pub async fn request_moderation(
    state: &AcademicState,
    auth: &AuthContext,
    window_id: Uuid,
    cmd: NewModeration,
    actor: &Actor,
) -> Result<AssessmentModeration, KernelError> {
    let tenant = auth.scope.tenant_id;
    let reason = required_text("reason", Some(&cmd.reason))?;
    crate::shared::faculty::require_teaching_role(auth, Permission::AssessmentManage)?;
    let w = get_window(state, tenant, window_id).await?;
    let c = teaching_ctx(state, auth, w.section_id, Permission::AssessmentManage).await?;
    if cmd.proposed_marks < 0.0 || cmd.proposed_marks > w.max_marks {
        return Err(invalid(
            "proposedMarks",
            "range",
            format!("marks are between 0 and {}", w.max_marks),
        ));
    }
    let s: SubmissionRow =
        kit::fetch(&state.pool, SB, "submission", tenant, cmd.submission_id).await?;
    if s.window_id != window_id {
        return Err(invalid(
            "submissionId",
            "window",
            "the submission belongs to another window",
        ));
    }
    let Some(original) = s
        .marks
        .filter(|_| s.status == AssessmentSubmissionStatus::Marked)
    else {
        return Err(KernelError::Conflict(format!(
            "only a marked submission is moderated (it is {})",
            s.status
        )));
    };
    let mut vars = BTreeMap::new();
    vars.insert("submissionId".to_owned(), json!(s.id));
    let outcome = approval::request_approval(
        state,
        &c.scope,
        process::ASSESSMENT_MODERATION,
        business_key(SB, s.id, s.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let m: AssessmentModeration = kit::insert(&mut tx, MO, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "window_id": window_id, "submission_id": s.id,
        "original_marks": original, "proposed_marks": cmd.proposed_marks, "reason": reason,
        "evidence": kit::opt_text(cmd.evidence.as_deref()), "process_instance_id": outcome.process_instance_id(), "requested_by": actor.id,
    }))
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("a moderation of this submission is already pending".into()),
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &c.scope,
        MO,
        "request",
        m.id,
        json!({"submissionId": s.id}),
        actor,
    )
    .await?;
    let m = if let ApprovalOutcome::Approved { .. } = outcome {
        apply_moderation(
            &mut tx,
            &c,
            &m,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        m
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(m)
}

#[allow(clippy::too_many_arguments)]
pub async fn decide_moderation(
    state: &AcademicState,
    auth: &AuthContext,
    window_id: Uuid,
    moderation_id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AssessmentModeration, KernelError> {
    auth.require(Permission::AssessmentManage)?;
    let tenant = auth.scope.tenant_id;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let w = get_window(state, tenant, window_id).await?;
    let c = kit::section_ctx(state, auth, w.section_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let m: AssessmentModeration =
        kit::lock(&mut tx, MO, "moderation", tenant, moderation_id).await?;
    if m.window_id != window_id {
        return Err(KernelError::NotFound {
            entity: "moderation",
        });
    }
    check_version(m.version, expected)?;
    if m.status != ChangeRequestStatus::Requested {
        return Err(KernelError::Conflict(format!(
            "the moderation is {}; it has no pending decision",
            m.status
        )));
    }
    // independent academic review (L4-D-054-BH-003)
    if m.requested_by.as_deref() == actor.id.as_deref() {
        return Err(KernelError::Forbidden(
            "the requester cannot decide their own moderation".into(),
        ));
    }
    let out = apply_moderation(&mut tx, &c, &m, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ finalisation

pub async fn list_results(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
) -> Result<Vec<InternalAssessmentResult>, KernelError> {
    kit::list_by(
        &state.pool,
        RS,
        tenant,
        "section_id",
        section,
        None,
        "t.student_number, t.result_version",
        5000,
    )
    .await
}

fn finalized_payload(r: &InternalAssessmentResult) -> InternalAssessmentFinalizedV1 {
    InternalAssessmentFinalizedV1 {
        result_id: r.id,
        section_id: r.section_id,
        offering_id: r.offering_id,
        institution_id: r.institution_id,
        student_id: r.student_id,
        academic_term_id: r.academic_term_id,
        curriculum_component_id: r.curriculum_component_id,
        marks_obtained: r.marks_obtained,
        max_marks: r.max_marks,
        version: i64::from(r.result_version),
    }
}

async fn finalize_rows(
    conn: &mut PgConnection,
    c: &SectionCtx,
    rows: Vec<InternalAssessmentResult>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let tenant = c.scope.tenant_id;
    for r in rows {
        let nr: InternalAssessmentResult = kit::patch(
            conn,
            RS,
            tenant,
            r.id,
            json!({"status": "finalized", "finalized_at": Utc::now(), "finalized_by": actor.id}),
        )
        .await?;
        publish::event(
            conn,
            &c.scope,
            INTERNAL_ASSESSMENT_FINALIZED,
            INTERNAL_ASSESSMENT_FINALIZED_SCHEMA,
            nr.id.to_string(),
            &finalized_payload(&nr),
            actor,
        )
        .await?;
    }
    sqlx::query("UPDATE academic.assessment_submission s SET status = 'finalized', version = s.version + 1, updated_at = now() \
                 FROM academic.assessment_window w WHERE w.id = s.window_id AND w.section_id = $1 AND s.status IN ('marked', 'moderated')")
        .bind(c.section.id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    kit::audit(
        conn,
        &c.scope,
        RS,
        "finalize",
        c.section.id.as_uuid(),
        json!({}),
        actor,
    )
    .await?;
    Ok(())
}

/// Assembles and finalises the section's internal assessment (`ASSESSMENT_FINALIZATION`).
pub async fn finalize(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    actor: &Actor,
) -> Result<Vec<InternalAssessmentResult>, KernelError> {
    auth.require(Permission::AssessmentFinalize)?;
    let tenant = auth.scope.tenant_id;
    let c = kit::section_ctx(state, auth, section).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    let bp: AssessmentBlueprint = {
        let v: Option<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM academic.assessment_blueprint t WHERE t.tenant_id = $1 AND t.section_id = $2 AND t.status = 'published'")
            .bind(tenant.as_uuid())
            .bind(section)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_db_error)?;
        v.map(kit::decode).transpose()?.ok_or_else(|| {
            KernelError::Conflict("the section has no published assessment blueprint".into())
        })?
    };
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.internal_assessment_result WHERE blueprint_id = $1 AND status IN ('in_progress', 'moderated', 'finalized', 'archived')")
        .bind(bp.id)
        .fetch_one(&state.pool)
        .await
        .map_err(map_db_error)?;
    if existing > 0 {
        return Err(KernelError::Conflict(
            "the internal assessment of this blueprint is already finalised or awaiting a decision"
                .into(),
        ));
    }
    let windows: Vec<AssessmentWindow> = kit::list_by(
        &state.pool,
        WN,
        tenant,
        "blueprint_id",
        bp.id,
        None,
        "t.due_at",
        500,
    )
    .await?;
    let windows: Vec<AssessmentWindow> = windows
        .into_iter()
        .filter(|w| w.status != AssessmentWindowStatus::Cancelled)
        .collect();
    let mut problems = Vec::new();
    if windows.is_empty() {
        problems.push("no assessment windows were held".to_owned());
    }
    let open = windows
        .iter()
        .filter(|w| w.status != AssessmentWindowStatus::Closed)
        .count();
    if open > 0 {
        problems.push(format!("{open} window(s) are not closed"));
    }
    let pending_mod: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.assessment_moderation m JOIN academic.assessment_window w ON w.id = m.window_id \
                                              WHERE w.blueprint_id = $1 AND m.status IN ('requested', 'validated', 'approved')")
        .bind(bp.id)
        .fetch_one(&state.pool)
        .await
        .map_err(map_db_error)?;
    if pending_mod > 0 {
        problems.push(format!("{pending_mod} moderation(s) are pending"));
    }
    let seated = roster(state, tenant, section).await?;
    let mut subs: Vec<(AssessmentWindow, Vec<SubmissionRow>)> = Vec::new();
    for w in &windows {
        subs.push((w.clone(), submission_rows(&state.pool, tenant, w.id).await?));
    }
    use AssessmentSubmissionStatus as S;
    for (w, rows) in &subs {
        let missing = seated
            .iter()
            .filter(|e| {
                !rows.iter().any(|r| {
                    r.student_id == e.student_id.as_uuid()
                        && matches!(
                            r.status,
                            S::Marked | S::Moderated | S::Absent | S::Finalized
                        )
                })
            })
            .count();
        if missing > 0 {
            problems.push(format!("{}: {missing} student(s) are not marked", w.title));
        }
        if bp.policy.moderation_required {
            let unmod = rows.iter().filter(|r| r.status == S::Marked).count();
            if unmod > 0 {
                problems.push(format!(
                    "{}: {unmod} submission(s) need moderation",
                    w.title
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(KernelError::Conflict(format!(
            "finalisation is blocked: {}",
            problems.join("; ")
        )));
    }
    let mut vars = BTreeMap::new();
    vars.insert("blueprintId".to_owned(), json!(bp.id));
    let outcome = approval::request_approval(
        state,
        &c.scope,
        process::ASSESSMENT_FINALIZATION,
        business_key(BP, bp.id, bp.version),
        vars,
    )
    .await?;
    let pending_status = if bp.policy.moderation_required {
        "moderated"
    } else {
        "in_progress"
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let mut rows = Vec::new();
    for e in &seated {
        let scores = |key: &str| -> Vec<Score> {
            subs.iter()
                .filter(|(w, _)| w.component_key == key)
                .map(|(w, rs)| Score {
                    obtained: rs
                        .iter()
                        .find(|r| r.student_id == e.student_id.as_uuid())
                        .and_then(SubmissionRow::effective)
                        .unwrap_or(0.0),
                    max: w.max_marks,
                })
                .collect()
        };
        let (lines, total) = domain::weighted_total(&bp.components, &scores, bp.total_marks);
        let r: InternalAssessmentResult = kit::insert(&mut tx, RS, json!({
            "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "offering_id": c.offering.id.as_uuid(),
            "section_id": section, "blueprint_id": bp.id, "academic_term_id": c.section.academic_term_id,
            "curriculum_component_id": c.offering.curriculum_component_id, "student_id": e.student_id.as_uuid(),
            "student_number": e.student_number, "display_name": e.display_name, "subject_ref": e.subject_ref,
            "component_marks": lines, "marks_obtained": total, "max_marks": bp.total_marks, "status": pending_status,
            "process_instance_id": outcome.process_instance_id(),
        })).await?;
        rows.push(r);
    }
    kit::audit(
        &mut tx,
        &c.scope,
        RS,
        "request_finalization",
        section,
        json!({"blueprintId": bp.id, "students": rows.len()}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        finalize_rows(
            &mut tx,
            &c,
            rows,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    list_results(state, tenant, section).await
}

/// Explicit finalisation decision (`AwaitDecision`): approve → finalized; reject → results withdrawn
/// (kept as `corrected` history rows would mislead, so they are archived as rejected attempts).
pub async fn decide_finalization(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Vec<InternalAssessmentResult>, KernelError> {
    auth.require(Permission::AssessmentFinalize)?;
    let tenant = auth.scope.tenant_id;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let c = kit::section_ctx(state, auth, section).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let pending: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM academic.internal_assessment_result t WHERE t.tenant_id = $1 AND t.section_id = $2 \
                                                             AND t.status IN ('in_progress', 'moderated') FOR UPDATE")
        .bind(tenant.as_uuid())
        .bind(section)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let pending: Vec<InternalAssessmentResult> = pending
        .into_iter()
        .map(kit::decode)
        .collect::<Result<_, _>>()?;
    if pending.is_empty() {
        return Err(KernelError::Conflict(
            "no internal assessment of this section is awaiting a finalisation decision".into(),
        ));
    }
    if approve {
        finalize_rows(&mut tx, &c, pending, actor).await?;
    } else {
        // a rejected finalisation leaves nothing official: the draft results are marked corrected
        // (superseded) so the section can be re-assembled after fixing the marks
        for r in pending {
            kit::patch::<InternalAssessmentResult>(
                &mut tx,
                RS,
                tenant,
                r.id,
                json!({"status": "corrected", "status_reason": reason}),
            )
            .await?;
        }
        kit::audit(
            &mut tx,
            &c.scope,
            RS,
            "finalization_reject",
            section,
            json!({"reason": reason}),
            actor,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    list_results(state, tenant, section).await
}

// ------------------------------------------------------------------ correction + archive

pub async fn list_corrections(
    state: &AcademicState,
    tenant: TenantId,
    result: Uuid,
) -> Result<Vec<AssessmentCorrection>, KernelError> {
    kit::list_by(
        &state.pool,
        CO,
        tenant,
        "result_id",
        result,
        None,
        "t.created_at DESC",
        200,
    )
    .await
}

async fn apply_correction(
    conn: &mut PgConnection,
    c: &SectionCtx,
    corr: &AssessmentCorrection,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AssessmentCorrection, KernelError> {
    let tenant = c.scope.tenant_id;
    if !approve {
        let out = kit::patch(conn, CO, tenant, corr.id, json!({"status": "rejected", "status_reason": reason, "decided_by": actor.id, "decided_at": Utc::now()})).await?;
        kit::audit(
            conn,
            &c.scope,
            CO,
            "reject",
            corr.id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        return Ok(out);
    }
    let orig: InternalAssessmentResult = kit::lock(
        conn,
        RS,
        "internal assessment result",
        tenant,
        corr.result_id,
    )
    .await?;
    if orig.status != InternalAssessmentStatus::Finalized {
        return Err(KernelError::Conflict(format!(
            "the result is {}; only a finalised result is corrected",
            orig.status
        )));
    }
    kit::patch::<InternalAssessmentResult>(
        conn,
        RS,
        tenant,
        orig.id,
        json!({"status": "corrected", "status_reason": corr.reason}),
    )
    .await?;
    let nr: InternalAssessmentResult = kit::insert(conn, RS, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": orig.institution_id, "offering_id": orig.offering_id, "section_id": orig.section_id,
        "blueprint_id": orig.blueprint_id, "academic_term_id": orig.academic_term_id, "curriculum_component_id": orig.curriculum_component_id,
        "student_id": orig.student_id, "student_number": orig.student_number, "display_name": orig.display_name, "subject_ref": orig.subject_ref,
        "component_marks": orig.component_marks, "marks_obtained": corr.to_marks, "max_marks": orig.max_marks, "status": "finalized",
        "result_version": orig.result_version + 1, "supersedes_id": orig.id, "process_instance_id": corr.process_instance_id,
        "finalized_at": Utc::now(), "finalized_by": actor.id,
    })).await?;
    let out: AssessmentCorrection = kit::patch(conn, CO, tenant, corr.id, json!({"status": "applied", "status_reason": reason, "corrected_result_id": nr.id, "decided_by": actor.id, "decided_at": Utc::now()})).await?;
    publish::event(
        conn,
        &c.scope,
        INTERNAL_ASSESSMENT_CORRECTED,
        INTERNAL_ASSESSMENT_CORRECTED_SCHEMA,
        nr.id.to_string(),
        &finalized_payload(&nr),
        actor,
    )
    .await?;
    kit::audit(
        conn,
        &c.scope,
        CO,
        "apply",
        corr.id,
        json!({"from": corr.from_marks, "to": corr.to_marks, "newResultId": nr.id}),
        actor,
    )
    .await?;
    Ok(out)
}

pub async fn request_correction(
    state: &AcademicState,
    auth: &AuthContext,
    result_id: Uuid,
    cmd: NewAssessmentCorrection,
    actor: &Actor,
) -> Result<AssessmentCorrection, KernelError> {
    auth.require(Permission::AssessmentFinalize)?;
    let tenant = auth.scope.tenant_id;
    let reason = required_text("reason", Some(&cmd.reason))?;
    let r: InternalAssessmentResult = kit::fetch(
        &state.pool,
        RS,
        "internal assessment result",
        tenant,
        result_id,
    )
    .await?;
    let c = kit::section_ctx(state, auth, r.section_id).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    if r.status != InternalAssessmentStatus::Finalized {
        return Err(KernelError::Conflict(format!(
            "the result is {}; only a finalised result is corrected",
            r.status
        )));
    }
    if cmd.marks_obtained < 0.0 || cmd.marks_obtained > r.max_marks {
        return Err(invalid(
            "marksObtained",
            "range",
            format!("marks are between 0 and {}", r.max_marks),
        ));
    }
    if (cmd.marks_obtained - r.marks_obtained).abs() < 0.005 {
        return Err(invalid(
            "marksObtained",
            "unchanged",
            "the result already has these marks",
        ));
    }
    let mut vars = BTreeMap::new();
    vars.insert("resultId".to_owned(), json!(r.id));
    let outcome = approval::request_approval(
        state,
        &c.scope,
        process::ASSESSMENT_CORRECTION,
        business_key(RS, r.id, r.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let corr: AssessmentCorrection = kit::insert(&mut tx, CO, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": r.institution_id, "result_id": r.id, "from_marks": r.marks_obtained,
        "to_marks": cmd.marks_obtained, "reason": reason, "evidence": kit::opt_text(cmd.evidence.as_deref()),
        "process_instance_id": outcome.process_instance_id(), "requested_by": actor.id,
    }))
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("a correction of this result is already pending".into()),
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &c.scope,
        CO,
        "request",
        corr.id,
        json!({"resultId": r.id}),
        actor,
    )
    .await?;
    let corr = if let ApprovalOutcome::Approved { .. } = outcome {
        apply_correction(
            &mut tx,
            &c,
            &corr,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        corr
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(corr)
}

#[allow(clippy::too_many_arguments)]
pub async fn decide_correction(
    state: &AcademicState,
    auth: &AuthContext,
    result_id: Uuid,
    correction_id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AssessmentCorrection, KernelError> {
    auth.require(Permission::AssessmentFinalize)?;
    let tenant = auth.scope.tenant_id;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let r: InternalAssessmentResult = kit::fetch(
        &state.pool,
        RS,
        "internal assessment result",
        tenant,
        result_id,
    )
    .await?;
    let c = kit::section_ctx(state, auth, r.section_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let corr: AssessmentCorrection =
        kit::lock(&mut tx, CO, "assessment correction", tenant, correction_id).await?;
    if corr.result_id != result_id {
        return Err(KernelError::NotFound {
            entity: "assessment correction",
        });
    }
    check_version(corr.version, expected)?;
    if corr.status != ChangeRequestStatus::Requested {
        return Err(KernelError::Conflict(format!(
            "the correction is {}; it has no pending decision",
            corr.status
        )));
    }
    let out = apply_correction(&mut tx, &c, &corr, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

pub async fn archive_result(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<InternalAssessmentResult, KernelError> {
    auth.require(Permission::AssessmentFinalize)?;
    let tenant = auth.scope.tenant_id;
    let cur: InternalAssessmentResult =
        kit::fetch(&state.pool, RS, "internal assessment result", tenant, id).await?;
    let c = kit::section_ctx(state, auth, cur.section_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let r: InternalAssessmentResult =
        kit::lock(&mut tx, RS, "internal assessment result", tenant, id).await?;
    check_version(r.version, expected)?;
    if !domain::result_can_transition(r.status, InternalAssessmentStatus::Archived) {
        return Err(illegal(
            "internal assessment",
            r.status,
            InternalAssessmentStatus::Archived,
        ));
    }
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.assessment_correction WHERE result_id = $1 AND status IN ('requested', 'validated', 'approved')")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if open > 0 {
        return Err(KernelError::Conflict(
            "a correction of this result is pending".into(),
        ));
    }
    let nr: InternalAssessmentResult =
        kit::patch(&mut tx, RS, tenant, id, json!({"status": "archived"})).await?;
    let mut e = status_event(&c, id, Some("finalized"), "archived", nr.version);
    e.student_id = Some(nr.student_id);
    e.emit(
        &mut tx,
        &c.scope,
        (
            INTERNAL_ASSESSMENT_ARCHIVED,
            INTERNAL_ASSESSMENT_ARCHIVED_SCHEMA,
        ),
        actor,
    )
    .await?;
    kit::audit(&mut tx, &c.scope, RS, "archive", id, json!({}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(nr)
}

// ------------------------------------------------------------------ student view

pub async fn my_assignments(
    state: &AcademicState,
    auth: &AuthContext,
) -> Result<MyAssignments, KernelError> {
    let (me, sections) = crate::shared::me::my_sections(state, auth).await?;
    let tenant = auth.scope.tenant_id;
    let mut items = Vec::new();
    for s in sections {
        let windows = list_windows(state, tenant, s.id.as_uuid(), None).await?;
        for w in windows.into_iter().filter(|w| {
            !matches!(
                w.status,
                AssessmentWindowStatus::Scheduled | AssessmentWindowStatus::Cancelled
            )
        }) {
            let sub: Option<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM academic.assessment_submission t WHERE t.window_id = $1 AND t.student_id = $2")
                .bind(w.id)
                .bind(me.id.as_uuid())
                .fetch_optional(&state.pool)
                .await
                .map_err(map_db_error)?;
            let sub: Option<SubmissionRow> = sub.map(kit::decode).transpose()?;
            items.push(MyAssignment {
                id: w.id,
                section_id: w.section_id,
                offering_code: s.offering_code.clone(),
                course_title: s.offering_title.clone(),
                title: w.title,
                kind: w.kind,
                instructions: w.instructions,
                opens_at: w.opens_at,
                due_at: w.due_at,
                extended_due_at: w.extended_due_at,
                max_marks: w.max_marks,
                status: w.status,
                submission: sub.map(|r| MySubmission {
                    status: r.status,
                    submitted_at: r.submitted_at,
                    marks: r.effective(),
                    feedback: r.feedback,
                }),
            });
        }
    }
    items.sort_by_key(|a| a.due_at);
    Ok(MyAssignments { items })
}
