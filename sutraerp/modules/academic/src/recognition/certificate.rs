//! External learning certificate verification (L3-D-043): evidence recorded against the right
//! learner / enrolment / provider; authenticity, learner identity and provider accreditation
//! checked; `accepted` only when every check succeeds, `rejected` with a reason, `flagged`
//! (fraud suspected — blocks recognition) on fraud indicators; `CERTIFICATE_VERIFICATION`
//! approval; the original evidence and verification history are kept for every outcome.

use crate::flexible::domain::certificate_outcome;
use crate::flexible::enrolment::{self, FlexibleEnrolment};
use crate::flexible::kit::{
    self, begin, check_version, commit, history, invalid, require, row_scope, FlexibleHistoryEntry,
    StatusEvent,
};
use crate::flexible::opportunity::{self, LearningOpportunity};
use crate::flexible::provider::{self, LearningProvider};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::status::{
    CertificateVerificationStatus as C, FlexibleEnrolmentStatus, LearningProviderStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "certificate_verification";
const ENTITY: &str = "certificate verification";

/// Verification checks (absent = not yet established).
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct CertificateChecks {
    pub authenticity_confirmed: Option<bool>,
    pub identity_matched: Option<bool>,
    /// Derived from the provider's status and accreditation validity when not given.
    pub provider_accredited: Option<bool>,
    #[serde(default)]
    pub fraud_indicators: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateVerification {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub enrolment_id: Uuid,
    pub provider_id: Option<Uuid>,
    pub student_id: Uuid,
    pub certificate_number: String,
    pub certificate_url: Option<String>,
    pub issued_on: Option<NaiveDate>,
    pub learner_name: Option<String>,
    pub status: C,
    pub checks: CertificateChecks,
    pub decision_reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub submitted_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CertificateVerificationList {
    pub items: Vec<CertificateVerification>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewCertificate {
    pub certificate_number: String,
    pub certificate_url: Option<String>,
    pub issued_on: Option<NaiveDate>,
    pub learner_name: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifyCertificate {
    pub authenticity_confirmed: Option<bool>,
    pub identity_matched: Option<bool>,
    pub provider_accredited: Option<bool>,
    #[serde(default)]
    pub fraud_indicators: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateDecision {
    /// `accepted` | `rejected` | `flagged`.
    pub outcome: C,
    pub reason: Option<String>,
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<CertificateVerification, KernelError> {
    let mut c: CertificateVerification = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    c.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(c)
}

pub async fn list_for_enrolment(
    st: &AcademicState,
    tenant: TenantId,
    enrolment: Uuid,
) -> Result<Vec<CertificateVerification>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "enrolment_id",
        enrolment,
        None,
        "t.created_at",
        100,
    )
    .await
}

pub async fn list_for_institution(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<CertificateVerification>, KernelError> {
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

/// BH-001: records the certificate evidence against the enrolment (registered or beyond).
pub async fn submit(
    st: &AcademicState,
    ctx: &AuthContext,
    enrolment_id: Uuid,
    cmd: NewCertificate,
    actor: &Actor,
) -> Result<CertificateVerification, KernelError> {
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
    require(
        matches!(
            e.status,
            FlexibleEnrolmentStatus::Registered
                | FlexibleEnrolmentStatus::InProgress
                | FlexibleEnrolmentStatus::Completed
        ),
        format!(
            "the flexible enrolment is {}; certificates are evidence of a registered participation",
            e.status
        ),
    )?;
    let number = kit::required_text("certificateNumber", Some(&cmd.certificate_number))?;
    // SEC-P1-APPSEC-004: absolute http(s) link with a host, no whitespace / control characters.
    let certificate_url =
        sutra_kernel::weblink::opt_http_link("certificateUrl", cmd.certificate_url.as_deref())?;
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        tenant,
        e.opportunity_id,
    )
    .await?;
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM academic.certificate_verification WHERE enrolment_id = $1 AND lower(certificate_number) = lower($2) AND status <> 'rejected')",
    )
    .bind(enrolment_id)
    .bind(&number)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    require(
        !dup,
        "this certificate is already recorded for the enrolment",
    )?;
    let mut tx = begin(st).await?;
    let c: CertificateVerification = kit::insert(&mut tx, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": e.institution_id, "enrolment_id": e.id,
        "provider_id": o.provider_id, "student_id": e.student_id, "certificate_number": number,
        "certificate_url": certificate_url, "issued_on": cmd.issued_on,
        "learner_name": kit::opt_text(cmd.learner_name.as_deref()), "submitted_by": actor.id,
    })).await?;
    history(
        &mut tx,
        tenant,
        TABLE,
        c.id,
        None,
        "submitted",
        None,
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "submit",
        c.id,
        json!({"enrolmentId": e.id}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, c.id).await
}

/// Moves a locked verification to a terminal outcome and emits `certificate_verification.decided`.
#[allow(clippy::too_many_arguments)]
async fn decide_in(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    c: &CertificateVerification,
    to: C,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<CertificateVerification, KernelError> {
    let n: CertificateVerification = kit::patch(conn, TABLE, tenant, c.id, json!({
        "status": to.as_str(), "decision_reason": reason, "decided_by": actor.id, "decided_at": kit::now(),
    })).await?;
    history(
        conn,
        tenant,
        TABLE,
        c.id,
        Some(c.status.as_str()),
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
        c.id,
        json!({"outcome": to, "reason": reason, "checks": c.checks}),
        actor,
    )
    .await?;
    let subject: Option<String> =
        sqlx::query_scalar("SELECT subject_ref FROM academic.flexible_enrolment WHERE id = $1")
            .bind(c.enrolment_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
    StatusEvent {
        aggregate_id: c.id,
        institution_id: c.institution_id,
        student_id: Some(c.student_id),
        from: Some(c.status.as_str()),
        to: to.as_str(),
        version: n.version,
        reason,
        process_instance_id: pid.or(n.process_instance_id.as_deref()),
        student_subject_ref: subject.as_deref(),
        ..Default::default()
    }
    .emit(
        conn,
        scope,
        (
            CERTIFICATE_VERIFICATION_DECIDED,
            CERTIFICATE_VERIFICATION_DECIDED_SCHEMA,
        ),
        actor,
    )
    .await?;
    Ok(n)
}

/// Provider accreditation on the issue date (status active + accreditation valid then).
async fn provider_accredited(
    st: &AcademicState,
    tenant: TenantId,
    c: &CertificateVerification,
) -> Result<Option<bool>, KernelError> {
    let Some(pid) = c.provider_id else {
        return Ok(Some(true));
    };
    let p: LearningProvider =
        kit::fetch(&st.pool, provider::TABLE, "learning provider", tenant, pid).await?;
    let on = c.issued_on.unwrap_or_else(|| Utc::now().date_naive());
    Ok(Some(
        p.status == LearningProviderStatus::Active
            && p.accreditation_valid_until.is_some_and(|d| d >= on),
    ))
}

/// Records the verification checks and starts `CERTIFICATE_VERIFICATION`; with auto-approval the
/// outcome derived from the checks is recorded at once (missing checks keep it under
/// verification; BH-003 no Accepted without every check).
pub async fn verify(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: VerifyCertificate,
    actor: &Actor,
) -> Result<CertificateVerification, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let c: CertificateVerification = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    check_version(c.version, expected)?;
    let scope = row_scope(ctx, c.institution_id);
    require_institution_writable(st, &scope, InstitutionId(c.institution_id)).await?;
    require(
        matches!(c.status, C::Submitted | C::UnderVerification),
        format!("the certificate verification is already {}", c.status),
    )?;
    let accredited = match cmd.provider_accredited {
        Some(v) => Some(v),
        None => provider_accredited(st, tenant, &c).await?,
    };
    let checks = CertificateChecks {
        authenticity_confirmed: cmd.authenticity_confirmed,
        identity_matched: cmd.identity_matched,
        provider_accredited: accredited,
        fraud_indicators: cmd
            .fraud_indicators
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
    };
    let derived = certificate_outcome(
        checks.authenticity_confirmed,
        checks.identity_matched,
        checks.provider_accredited,
        &checks.fraud_indicators,
    );
    let outcome = if c.status == C::Submitted {
        let mut vars = BTreeMap::new();
        vars.insert("certificateNumber".into(), json!(c.certificate_number));
        Some(
            approval::request_approval(
                st,
                &scope,
                process::CERTIFICATE_VERIFICATION,
                business_key(TABLE, c.id, c.version),
                vars,
            )
            .await?,
        )
    } else {
        None
    };
    let mut tx = begin(st).await?;
    let locked: CertificateVerification = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let mut changes = json!({"checks": checks, "status": "under_verification"});
    if let Some(o) = &outcome {
        changes["process_instance_id"] = json!(o.process_instance_id());
    }
    let n: CertificateVerification = kit::patch(&mut tx, TABLE, tenant, id, changes).await?;
    if locked.status != C::UnderVerification {
        history(
            &mut tx,
            tenant,
            TABLE,
            id,
            Some(locked.status.as_str()),
            "under_verification",
            None,
            n.process_instance_id.as_deref(),
            actor,
        )
        .await?;
    }
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "verify",
        id,
        json!({"checks": n.checks}),
        actor,
    )
    .await?;
    let auto = matches!(outcome, Some(ApprovalOutcome::Approved { .. }))
        || (outcome.is_none() && st.approvals == approval::ApprovalMode::AutoApprove);
    if auto {
        if let Some(to) = derived {
            let reason = kit::opt_text(cmd.reason.as_deref()).or_else(|| match to {
                "flagged" => Some(format!(
                    "fraud suspected: {}",
                    checks.fraud_indicators.join("; ")
                )),
                "rejected" => Some("verification failed: a required check did not succeed".into()),
                _ => None,
            });
            let sys = Actor::system(actor.correlation_id.clone());
            let to = C::parse(to).ok_or_else(|| KernelError::internal_msg("bad outcome"))?;
            decide_in(
                &mut tx,
                &scope,
                tenant,
                &n,
                to,
                reason.as_deref(),
                n.process_instance_id.clone().as_deref(),
                &sys,
            )
            .await?;
        }
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Explicit verification decision (`AwaitDecision`, or a compliance decision on a case the
/// checks leave open). `accepted` needs every check confirmed and no fraud indicator;
/// `rejected` / `flagged` need a reason.
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: CertificateDecision,
    actor: &Actor,
) -> Result<CertificateVerification, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let c: CertificateVerification = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    check_version(c.version, expected)?;
    let scope = row_scope(ctx, c.institution_id);
    require_institution_writable(st, &scope, InstitutionId(c.institution_id)).await?;
    require(
        c.status == C::UnderVerification,
        format!(
            "the certificate verification is {}; record the checks first",
            c.status
        ),
    )?;
    let reason = kit::opt_text(cmd.reason.as_deref());
    match cmd.outcome {
        C::Accepted => require(
            certificate_outcome(c.checks.authenticity_confirmed, c.checks.identity_matched, c.checks.provider_accredited, &c.checks.fraud_indicators) == Some("accepted"),
            "a certificate is accepted only when authenticity, identity and provider accreditation are confirmed and no fraud indicator exists",
        )?,
        C::Rejected | C::Flagged if reason.is_none() => return Err(invalid("reason", "required", "a rejection or fraud flag needs a reason")),
        C::Rejected | C::Flagged => {}
        other => return Err(invalid("outcome", "invalid", format!("{other} is not a verification outcome"))),
    }
    let mut tx = begin(st).await?;
    let locked: CertificateVerification = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    decide_in(
        &mut tx,
        &scope,
        tenant,
        &locked,
        cmd.outcome,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// The accepted certificate of an enrolment (latest), if any.
pub async fn accepted_for(
    conn: &mut PgConnection,
    enrolment: Uuid,
) -> Result<Option<CertificateVerification>, KernelError> {
    let v: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.certificate_verification t WHERE t.enrolment_id = $1 AND t.status = 'accepted' ORDER BY t.decided_at DESC LIMIT 1",
    )
    .bind(enrolment)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    v.map(kit::decode).transpose()
}

/// `true` when a certificate of the enrolment is flagged (fraud suspected → recognition blocked).
pub async fn flagged(conn: &mut PgConnection, enrolment: Uuid) -> Result<bool, KernelError> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM academic.certificate_verification WHERE enrolment_id = $1 AND status = 'flagged')")
        .bind(enrolment)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)
}
