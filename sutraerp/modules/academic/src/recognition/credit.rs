//! Credit recognition and posting (L3-D-039): only a completed participation with an accepted
//! (never flagged) certificate and an effective mapping rule opens a recognition; the governed
//! academic decision (`CREDIT_RECOGNITION`) records approved / partially approved / rejected;
//! an approved recognition is posted into the Domain C credit ledger **only** through
//! `CreditPostingPort` (idempotent on the recognition id), outside our transaction, and the
//! receipt is then recorded (append-only posting history + `credit_recognition.posted`). The
//! transcript / ledger is never written directly.

use super::certificate;
use super::mapping;
use crate::flexible::enrolment::{self, FlexibleEnrolment};
use crate::flexible::kit::{
    self, begin, check_version, commit, history, invalid, require, row_scope, FlexibleHistoryEntry,
    StatusEvent,
};
use crate::flexible::opportunity::{self, LearningOpportunity};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::{publish, AcademicState, Actor};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::status::{CreditRecognitionStatus as R, FlexibleEnrolmentStatus};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{CreditPostingRequest, CREDIT_SOURCE_FLEXIBLE_LEARNING};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "credit_recognition";
const ENTITY: &str = "credit recognition";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditRecognitionPosting {
    pub id: Uuid,
    pub recognition_id: Uuid,
    pub ledger_entry_id: Uuid,
    pub already_posted: bool,
    pub credits: f64,
    pub posted_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditRecognition {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub flexible_enrolment_id: Uuid,
    pub certificate_verification_id: Uuid,
    pub mapping_rule_id: Uuid,
    pub opportunity_id: Uuid,
    pub student_id: Uuid,
    pub student_subject_ref: Option<String>,
    pub academic_term_id: Option<Uuid>,
    pub curriculum_component_id: Option<Uuid>,
    pub requested_credits: f64,
    /// Approved credits (≤ requested; partial approval).
    pub credits: f64,
    pub status: R,
    pub decision_reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    /// `student.credit_ledger_entry` id returned by `CreditPostingPort` once posted.
    pub ledger_entry_id: Option<Uuid>,
    pub posted_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub postings: Vec<CreditRecognitionPosting>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CreditRecognitionList {
    pub items: Vec<CreditRecognition>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewCreditRecognition {
    /// Defaults to the enrolment's latest accepted certificate.
    pub certificate_verification_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditRecognitionDecision {
    /// `approve` | `reject`.
    pub decision: kit::FlexibleDecisionValue,
    /// Partial approval: approved credits (≤ requested). Defaults to the requested credits.
    pub credits: Option<f64>,
    pub reason: Option<String>,
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<CreditRecognition, KernelError> {
    let mut r: CreditRecognition = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    r.postings = kit::list_by(
        &st.pool,
        "credit_recognition_posting",
        tenant,
        "recognition_id",
        id,
        None,
        "t.created_at",
        50,
    )
    .await?;
    r.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(r)
}

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<CreditRecognition>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "institution_id",
        institution,
        status,
        "t.created_at DESC",
        200,
    )
    .await
}

pub async fn list_for_student(
    st: &AcademicState,
    tenant: TenantId,
    student: Uuid,
) -> Result<Vec<CreditRecognition>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "student_id",
        student,
        None,
        "t.created_at DESC",
        100,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &CreditRecognition,
    to: R,
    reason: Option<&str>,
    pid: Option<&str>,
    extra: serde_json::Value,
    actor: &Actor,
) -> Result<CreditRecognition, KernelError> {
    let legal = matches!(
        (r.status, to),
        (R::Requested, R::UnderReview)
            | (R::UnderReview, R::Approved | R::Rejected)
            | (R::Approved, R::Posted)
    );
    if !legal {
        return Err(kit::illegal(ENTITY, r.status, to));
    }
    let mut changes = json!({"status": to.as_str()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    if let (serde_json::Value::Object(m), serde_json::Value::Object(x)) = (&mut changes, extra) {
        m.extend(x);
    }
    let n: CreditRecognition = kit::patch(conn, TABLE, tenant, r.id, changes).await?;
    history(
        conn,
        tenant,
        TABLE,
        r.id,
        Some(r.status.as_str()),
        to.as_str(),
        reason,
        pid,
        actor,
    )
    .await?;
    kit::audit(
        conn,
        scope,
        TABLE,
        to.as_str(),
        r.id,
        json!({"from": r.status, "to": to, "reason": reason, "credits": n.credits}),
        actor,
    )
    .await?;
    if to == R::Approved {
        StatusEvent {
            aggregate_id: r.id,
            institution_id: r.institution_id,
            student_id: Some(r.student_id),
            from: Some(r.status.as_str()),
            to: to.as_str(),
            version: n.version,
            reason,
            process_instance_id: pid.or(n.process_instance_id.as_deref()),
            student_subject_ref: r.student_subject_ref.as_deref(),
            ..Default::default()
        }
        .emit(
            conn,
            scope,
            (
                CREDIT_RECOGNITION_APPROVED,
                CREDIT_RECOGNITION_APPROVED_SCHEMA,
            ),
            actor,
        )
        .await?;
    }
    Ok(n)
}

/// Opens the recognition for a completed participation (BH-001…004, 009) and starts the
/// governed review; a repeat for the same enrolment returns the live recognition.
pub async fn request(
    st: &AcademicState,
    ctx: &AuthContext,
    enrolment_id: Uuid,
    cmd: NewCreditRecognition,
    actor: &Actor,
) -> Result<(CreditRecognition, bool), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let e: FlexibleEnrolment = kit::fetch(
        &st.pool,
        enrolment::TABLE,
        enrolment::ENTITY,
        tenant,
        enrolment_id,
    )
    .await?;
    let scope = row_scope(ctx, e.institution_id);
    require_institution_writable(st, &scope, InstitutionId(e.institution_id)).await?;
    let live: Option<Uuid> = sqlx::query_scalar("SELECT id FROM academic.credit_recognition WHERE flexible_enrolment_id = $1 AND status <> 'rejected'")
        .bind(enrolment_id)
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
    if let Some(id) = live {
        return Ok((get(st, tenant, id).await?, false));
    }
    require(
        e.status == FlexibleEnrolmentStatus::Completed,
        format!(
            "the flexible enrolment is {}; recognition needs a recorded learning completion",
            e.status
        ),
    )?;
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        tenant,
        e.opportunity_id,
    )
    .await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    require(
        !certificate::flagged(&mut conn, enrolment_id).await?,
        "a certificate of this enrolment is flagged (fraud suspected); recognition is blocked",
    )?;
    let cert = match cmd.certificate_verification_id {
        Some(cid) => {
            let c: certificate::CertificateVerification = kit::fetch(&mut *conn, certificate::TABLE, "certificate verification", tenant, cid).await?;
            if c.enrolment_id != enrolment_id {
                return Err(invalid("certificateVerificationId", "mismatch", "the certificate belongs to another enrolment"));
            }
            require(c.status.as_str() == "accepted", format!("the certificate verification is {}; recognition needs an accepted certificate", c.status))?;
            c
        }
        None => certificate::accepted_for(&mut conn, enrolment_id)
            .await?
            .ok_or_else(|| KernelError::Conflict("no accepted certificate for this enrolment; recognition needs verified evidence".into()))?,
    };
    let rule = mapping::effective_for(&mut conn, o.id)
        .await?
        .ok_or_else(|| {
            KernelError::Conflict("no effective credit mapping rule for this opportunity".into())
        })?;
    drop(conn);
    let id = Uuid::now_v7();
    let mut vars = BTreeMap::new();
    vars.insert("studentNumber".into(), json!(e.student_number));
    vars.insert("credits".into(), json!(rule.credits));
    let outcome = approval::request_approval(
        st,
        &scope,
        process::CREDIT_RECOGNITION,
        business_key(TABLE, id, 1),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(st).await?;
    let r: CreditRecognition = kit::insert(&mut tx, TABLE, json!({
        "id": id, "tenant_id": tenant.as_uuid(), "institution_id": e.institution_id, "flexible_enrolment_id": e.id,
        "certificate_verification_id": cert.id, "mapping_rule_id": rule.id, "opportunity_id": o.id,
        "student_id": e.student_id, "student_subject_ref": e.subject_ref, "academic_term_id": o.academic_term_id,
        "curriculum_component_id": rule.curriculum_component_id, "requested_credits": rule.credits, "credits": rule.credits,
    })).await?;
    history(
        &mut tx,
        tenant,
        TABLE,
        id,
        None,
        "requested",
        None,
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "request",
        id,
        json!({"enrolmentId": e.id, "mappingRuleId": rule.id, "credits": rule.credits}),
        actor,
    )
    .await?;
    let n = transition(
        &mut tx,
        &scope,
        tenant,
        &r,
        R::UnderReview,
        None,
        Some(&pid),
        json!({}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(actor.correlation_id.clone());
        transition(
            &mut tx,
            &scope,
            tenant,
            &n,
            R::Approved,
            Some("recognition approved"),
            Some(&pid),
            json!({"decided_by": sys.id}),
            &sys,
        )
        .await?;
    }
    commit(tx).await?;
    Ok((get(st, tenant, id).await?, true))
}

/// Governed recognition decision: approve (optionally partial credits) or reject (reason).
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: CreditRecognitionDecision,
    actor: &Actor,
) -> Result<CreditRecognition, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let r: CreditRecognition = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    check_version(r.version, expected)?;
    let scope = row_scope(ctx, r.institution_id);
    require_institution_writable(st, &scope, InstitutionId(r.institution_id)).await?;
    require(
        r.status == R::UnderReview,
        format!(
            "the credit recognition is {}; there is no pending decision",
            r.status
        ),
    )?;
    let reason = kit::opt_text(cmd.reason.as_deref());
    let approve = cmd.decision == kit::FlexibleDecisionValue::Approve;
    let credits = cmd.credits.unwrap_or(r.requested_credits);
    if approve && !(credits > 0.0 && credits <= r.requested_credits) {
        return Err(invalid(
            "credits",
            "range",
            format!(
                "approved credits must be above 0 and at most {}",
                r.requested_credits
            ),
        ));
    }
    if approve && credits < r.requested_credits && reason.is_none() {
        return Err(invalid(
            "reason",
            "required",
            "a partial approval needs a reason",
        ));
    }
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = begin(st).await?;
    let locked: CreditRecognition = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let extra = if approve {
        json!({"credits": credits, "decision_reason": reason, "decided_by": actor.id})
    } else {
        json!({"decision_reason": reason, "decided_by": actor.id})
    };
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        if approve { R::Approved } else { R::Rejected },
        reason.as_deref(),
        None,
        extra,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Posts an approved recognition into the Domain C ledger through `CreditPostingPort` (after
/// the decision, outside our transaction; idempotent on the recognition id), then records the
/// receipt. A repeat on a posted recognition returns it unchanged.
pub async fn post(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<CreditRecognition, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let r: CreditRecognition = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    if r.status == R::Posted {
        return get(st, tenant, id).await;
    }
    check_version(r.version, expected)?;
    let scope = row_scope(ctx, r.institution_id);
    require_institution_writable(st, &scope, InstitutionId(r.institution_id)).await?;
    require(
        r.status == R::Approved,
        format!(
            "the credit recognition is {}; only approved recognitions are posted",
            r.status
        ),
    )?;
    let receipt = st
        .credits
        .post_credits(
            &scope,
            CreditPostingRequest {
                institution_id: InstitutionId(r.institution_id),
                student_id: StudentId(r.student_id),
                credits: r.credits,
                curriculum_component_id: r.curriculum_component_id,
                academic_term_id: r.academic_term_id,
                source_type: CREDIT_SOURCE_FLEXIBLE_LEARNING,
                source_id: r.id,
                note: Some(format!("flexible learning recognition {}", r.id)),
                actor: actor.id.clone(),
            },
        )
        .await?;
    let mut tx = begin(st).await?;
    let locked: CreditRecognition = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    kit::append(&mut tx, "credit_recognition_posting", json!({
        "tenant_id": tenant.as_uuid(), "recognition_id": id, "ledger_entry_id": receipt.ledger_entry_id,
        "already_posted": receipt.already_posted, "credits": locked.credits, "posted_by": actor.id,
    })).await?;
    if locked.status == R::Approved {
        let n = transition(
            &mut tx,
            &scope,
            tenant,
            &locked,
            R::Posted,
            None,
            None,
            json!({"ledger_entry_id": receipt.ledger_entry_id, "posted_at": kit::now()}),
            actor,
        )
        .await?;
        publish::event(
            &mut tx,
            &scope,
            CREDIT_RECOGNITION_POSTED,
            CREDIT_RECOGNITION_POSTED_SCHEMA,
            id.to_string(),
            &CreditRecognitionPostedV1 {
                recognition_id: id,
                institution_id: n.institution_id,
                student_id: n.student_id,
                credits: n.credits,
                curriculum_component_id: n.curriculum_component_id,
                ledger_entry_id: receipt.ledger_entry_id,
                student_subject_ref: n.student_subject_ref.clone(),
            },
            actor,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}
